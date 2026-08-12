use monty::RunProgress;
use monty_fs::MountCallOutcome;
use monty_types::{
    ExcType, ExtFunctionResult, MontyException, MontyObject, NameLookupResult, OsFunctionCall,
    PrintWriter, ResourceTracker,
};
use rustler::types::atom::Atom;
use rustler::{Encoder, Env, NifResult, ResourceArc, Term};
use std::collections::HashSet;

use crate::error;
use crate::mounts::MountLease;
use crate::output::{OutputBudget, OutputCollector};
use crate::resources::{
    FutureSnapshotResource, RunnerResource, SnapshotKind, SnapshotResource, SnapshotTag,
};
use crate::types;

/// A resume result decoded for a specific snapshot variant, produced *before*
/// the snapshot is consumed so a malformed result can't destroy it.
enum DecodedResume {
    Ext(ExtFunctionResult),
    Name(NameLookupResult),
    Pending,
}

fn decode_resume_result<'a>(
    env: Env<'a>,
    kind: SnapshotTag,
    result: Term<'a>,
) -> NifResult<DecodedResume> {
    match kind {
        SnapshotTag::NameLookup => Ok(DecodedResume::Name(decode_name_lookup_result(env, result)?)),
        SnapshotTag::FunctionCall if is_pending_atom(result) => Ok(DecodedResume::Pending),
        SnapshotTag::FunctionCall | SnapshotTag::OsCall => {
            Ok(DecodedResume::Ext(decode_external_result(env, result)?))
        }
    }
}

fn apply_resume(
    snap: SnapshotKind,
    decoded: DecodedResume,
    print: PrintWriter,
) -> Result<RunProgress, MontyException> {
    match (snap, decoded) {
        (SnapshotKind::FunctionCall(call), DecodedResume::Ext(r)) => call.resume(r, print),
        (SnapshotKind::FunctionCall(call), DecodedResume::Pending) => call.resume_pending(print),
        (SnapshotKind::OsCall(call), DecodedResume::Ext(r)) => call.resume(r, print),
        (SnapshotKind::NameLookup(lookup), DecodedResume::Name(r)) => lookup.resume(r, print),
        // The snapshot variant is fixed once created and `take` is one-shot, so
        // the kind we peeked always matches the kind we took — these arms are
        // unreachable. Return a clean error rather than panic, just in case.
        _ => Err(MontyException::new(
            ExcType::RuntimeError,
            Some("snapshot kind/result mismatch".to_string()),
        )),
    }
}

fn snapshot_consumed_error() -> rustler::Error {
    rustler::Error::RaiseTerm(Box::new("snapshot already consumed"))
}

#[rustler::nif(schedule = "DirtyCpu")]
fn start<'a>(
    env: Env<'a>,
    runner: ResourceArc<RunnerResource>,
    inputs: Vec<(String, Term<'a>)>,
    limits: Term<'a>,
) -> NifResult<Term<'a>> {
    let monty_run = runner.clone_runner();
    let monty_inputs = types::decode_inputs(env, inputs, runner.input_names())?;
    let resource_limits = types::decode_resource_limits(limits)?;
    let output_budget = OutputBudget::from_limits(&resource_limits);
    let tracker = ResourceTracker::new(resource_limits);
    let mut output = output_budget.collector();

    let progress = monty_run
        .start(monty_inputs, tracker, PrintWriter::Callback(&mut output))
        .map_err(error::monty_exception_to_rustler_error)?;

    encode_run_progress(env, progress, output)
}

#[rustler::nif(schedule = "DirtyCpu")]
fn resume<'a>(
    env: Env<'a>,
    snapshot: ResourceArc<SnapshotResource>,
    result: Term<'a>,
) -> NifResult<Term<'a>> {
    // Decode the (attacker-controlled) result *before* consuming the snapshot,
    // so a malformed result returns an error and leaves the snapshot intact for
    // a retry instead of silently destroying it.
    let kind = snapshot.peek_kind().ok_or_else(snapshot_consumed_error)?;
    let decoded = decode_resume_result(env, kind, result)?;

    let state = snapshot.take().ok_or_else(snapshot_consumed_error)?;

    let mut output = state.output_budget.collector();
    let print = PrintWriter::Callback(&mut output);

    let progress = apply_resume(state.snapshot, decoded, print)
        .map_err(error::monty_exception_to_rustler_error)?;

    encode_run_progress(env, progress, output)
}

#[rustler::nif(schedule = "DirtyCpu")]
fn resume_futures<'a>(
    env: Env<'a>,
    futures: ResourceArc<FutureSnapshotResource>,
    results: Vec<(u32, Term<'a>)>,
) -> NifResult<Term<'a>> {
    validate_future_result_ids(&futures, &results)?;

    // Decode all (attacker-controlled) results before consuming the snapshot,
    // so a malformed result leaves it intact for a retry.
    let external_results: Vec<(u32, ExtFunctionResult)> = results
        .into_iter()
        .map(|(id, term)| {
            let result = decode_external_result(env, term)?;
            Ok((id, result))
        })
        .collect::<NifResult<Vec<_>>>()?;

    let state = futures
        .take()
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("future snapshot already consumed")))?;

    let mut output = state.output_budget.collector();

    let progress = state
        .snapshot
        .resume(external_results, PrintWriter::Callback(&mut output))
        .map_err(error::monty_exception_to_rustler_error)?;

    encode_run_progress(env, progress, output)
}

#[rustler::nif(schedule = "DirtyCpu")]
fn pending_call_ids(futures: ResourceArc<FutureSnapshotResource>) -> NifResult<Vec<u32>> {
    futures
        .with(|snap| snap.pending_call_ids().to_vec())
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("future snapshot already consumed")))
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn encode_run_progress<'a>(
    env: Env<'a>,
    progress: RunProgress,
    output: OutputCollector,
) -> NifResult<Term<'a>> {
    let (output, output_budget) = output.finish();
    let output_term = output.encode(env);

    match progress {
        RunProgress::FunctionCall(call) => {
            let tag = if call.method_call {
                Atom::from_str(env, "method_call").unwrap()
            } else {
                Atom::from_str(env, "function_call").unwrap()
            };
            let call_term = encode_function_call(
                env,
                &call.function_name,
                &call.args,
                &call.kwargs,
                call.call_id,
            )?;
            let snapshot_ref = ResourceArc::new(SnapshotResource::new(
                SnapshotKind::FunctionCall(call),
                output_budget,
            ));
            Ok(rustler::types::tuple::make_tuple(
                env,
                &[
                    tag.encode(env),
                    call_term,
                    snapshot_ref.encode(env),
                    output_term,
                ],
            ))
        }
        RunProgress::OsCall(call) => {
            let tag = Atom::from_str(env, "os_call").unwrap();
            // `to_args` consumes the call value, so clone it for the Elixir-facing
            // view and keep the original in the snapshot for resume/on_no_handler.
            let (args, kwargs) = call.function_call.clone().to_args();
            let call_term = encode_os_call(env, &call.function_call, &args, &kwargs, call.call_id)?;
            let snapshot_ref = ResourceArc::new(SnapshotResource::new(
                SnapshotKind::OsCall(call),
                output_budget,
            ));
            Ok(rustler::types::tuple::make_tuple(
                env,
                &[
                    tag.encode(env),
                    call_term,
                    snapshot_ref.encode(env),
                    output_term,
                ],
            ))
        }
        RunProgress::NameLookup(lookup) => {
            let tag = Atom::from_str(env, "name_lookup").unwrap();
            let name_term = lookup.name.encode(env);
            let snapshot_ref = ResourceArc::new(SnapshotResource::new(
                SnapshotKind::NameLookup(lookup),
                output_budget,
            ));
            Ok(rustler::types::tuple::make_tuple(
                env,
                &[
                    tag.encode(env),
                    name_term,
                    snapshot_ref.encode(env),
                    output_term,
                ],
            ))
        }
        RunProgress::ResolveFutures(future_snapshot) => {
            let tag = Atom::from_str(env, "resolve_futures").unwrap();
            let futures_ref =
                ResourceArc::new(FutureSnapshotResource::new(future_snapshot, output_budget));
            Ok(rustler::types::tuple::make_tuple(
                env,
                &[tag.encode(env), futures_ref.encode(env), output_term],
            ))
        }
        RunProgress::Complete(value) => {
            let tag = Atom::from_str(env, "complete").unwrap();
            let value_term = types::encode_monty_object(env, &value)?;
            Ok(rustler::types::tuple::make_tuple(
                env,
                &[tag.encode(env), value_term, output_term],
            ))
        }
    }
}

fn encode_function_call<'a>(
    env: Env<'a>,
    name: &str,
    args: &[MontyObject],
    kwargs: &[(MontyObject, MontyObject)],
    call_id: u32,
) -> NifResult<Term<'a>> {
    let struct_atom = Atom::from_str(env, "Elixir.ExMonty.FunctionCall").unwrap();

    let args_term: Vec<Term> = args
        .iter()
        .map(|a| types::encode_monty_object(env, a))
        .collect::<NifResult<Vec<_>>>()?;
    let kwargs_term = encode_kwargs(env, kwargs)?;

    Ok(rustler::types::map::map_new(env)
        .map_put(
            Atom::from_str(env, "__struct__").unwrap().encode(env),
            struct_atom.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "name").unwrap().encode(env),
            name.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "args").unwrap().encode(env),
            args_term.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "kwargs").unwrap().encode(env),
            kwargs_term,
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "call_id").unwrap().encode(env),
            call_id.encode(env),
        )
        .unwrap())
}

fn encode_os_call<'a>(
    env: Env<'a>,
    function: &OsFunctionCall,
    args: &[MontyObject],
    kwargs: &[(MontyObject, MontyObject)],
    call_id: u32,
) -> NifResult<Term<'a>> {
    let struct_atom = Atom::from_str(env, "Elixir.ExMonty.OsCall").unwrap();

    let func_term = types::encode_os_function(env, function);
    let args_term: Vec<Term> = args
        .iter()
        .map(|a| types::encode_monty_object(env, a))
        .collect::<NifResult<Vec<_>>>()?;
    let kwargs_term = encode_kwargs(env, kwargs)?;

    Ok(rustler::types::map::map_new(env)
        .map_put(
            Atom::from_str(env, "__struct__").unwrap().encode(env),
            struct_atom.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "function").unwrap().encode(env),
            func_term,
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "args").unwrap().encode(env),
            args_term.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "kwargs").unwrap().encode(env),
            kwargs_term,
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "call_id").unwrap().encode(env),
            call_id.encode(env),
        )
        .unwrap())
}

fn encode_kwargs<'a>(env: Env<'a>, kwargs: &[(MontyObject, MontyObject)]) -> NifResult<Term<'a>> {
    let mut map = rustler::types::map::map_new(env);
    for (k, v) in kwargs {
        // kwargs keys are typically strings in Python
        let key = types::encode_monty_object(env, k)?;
        let val = types::encode_monty_object(env, v)?;
        map = map.map_put(key, val).unwrap();
    }
    Ok(map)
}

fn decode_external_result<'a>(env: Env<'a>, term: Term<'a>) -> NifResult<ExtFunctionResult> {
    use rustler::types::tuple::get_tuple;

    let elements = get_tuple(term).map_err(|_| invalid_callback_result())?;
    let tag = elements
        .first()
        .ok_or_else(invalid_callback_result)?
        .atom_to_string()
        .map_err(|_| invalid_callback_result())?;

    match (tag.as_str(), elements.as_slice()) {
        ("ok", [_, value]) => Ok(ExtFunctionResult::Return(types::decode_monty_object(
            env, *value,
        )?)),
        ("error", [_, exc_type, message]) => {
            let type_str = decode_atom_or_string(*exc_type)?;
            let msg: String = message.decode().map_err(|_| invalid_callback_result())?;
            let exc_type = parse_exc_type(&type_str)?;
            Ok(ExtFunctionResult::Error(MontyException::new(
                exc_type,
                Some(msg),
            )))
        }
        _ => Err(invalid_callback_result()),
    }
}

fn decode_name_lookup_result<'a>(env: Env<'a>, term: Term<'a>) -> NifResult<NameLookupResult> {
    // :undefined → Undefined
    if term.is_atom() {
        if let Ok(s) = term.atom_to_string() {
            if s == "undefined" {
                return Ok(NameLookupResult::Undefined);
            }
        }
    }

    // {:ok, value} → Value(obj)
    if let Ok(elements) = rustler::types::tuple::get_tuple(term) {
        if elements.len() == 2 {
            if let Ok(tag) = elements[0].atom_to_string() {
                if tag == "ok" {
                    let obj = types::decode_monty_object(env, elements[1])?;
                    return Ok(NameLookupResult::Value(obj));
                }
            }
        }
    }

    Err(rustler::Error::Term(Box::new(
        "name lookup result must be :undefined or {:ok, value}",
    )))
}

fn parse_exc_type(s: &str) -> NifResult<ExcType> {
    // Try parsing both snake_case and PascalCase
    use std::str::FromStr;
    if let Ok(t) = ExcType::from_str(s) {
        return Ok(t);
    }
    // Try converting from snake_case to PascalCase
    let pascal = s
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(c) => {
                    let upper: String = c.to_uppercase().collect();
                    upper + &chars.collect::<String>()
                }
                None => String::new(),
            }
        })
        .collect::<String>();
    if let Ok(t) = ExcType::from_str(&pascal) {
        return Ok(t);
    }

    // Acronym-preserving aliases produced by the Elixir-facing snake_case
    // convention cannot be reconstructed by the generic PascalCase helper.
    match s {
        "os_error" | "o_s_error" => Ok(ExcType::OSError),
        _ => Err(rustler::Error::Term(Box::new(format!(
            "unknown exception type: {s}"
        )))),
    }
}

fn decode_atom_or_string(term: Term<'_>) -> NifResult<String> {
    if term.is_atom() {
        term.atom_to_string().map_err(|_| invalid_callback_result())
    } else {
        term.decode().map_err(|_| invalid_callback_result())
    }
}

fn invalid_callback_result() -> rustler::Error {
    rustler::Error::Term(Box::new(
        "callback result must be {:ok, value} or {:error, exception_type, message}",
    ))
}

fn is_pending_atom(term: Term<'_>) -> bool {
    term.is_atom() && matches!(term.atom_to_string().as_deref(), Ok("pending"))
}

fn validate_future_result_ids(
    futures: &FutureSnapshotResource,
    results: &[(u32, Term<'_>)],
) -> NifResult<()> {
    let pending = futures
        .with(|snap| snap.pending_call_ids().to_vec())
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("future snapshot already consumed")))?;
    if results.len() > pending.len() {
        return Err(rustler::Error::Term(Box::new(format!(
            "too many future results: got {}, at most {} are pending",
            results.len(),
            pending.len()
        ))));
    }
    let pending: HashSet<u32> = pending.into_iter().collect();
    let mut seen = HashSet::with_capacity(results.len());

    for (call_id, _) in results {
        if !pending.contains(call_id) {
            return Err(rustler::Error::Term(Box::new(format!(
                "unknown future call_id: {call_id}"
            ))));
        }
        if !seen.insert(*call_id) {
            return Err(rustler::Error::Term(Box::new(format!(
                "duplicate future call_id: {call_id}"
            ))));
        }
    }

    Ok(())
}

// ── Mount-aware variants ─────────────────────────────────────────────────
//
// These mirror upstream's `drive_run_progress_through_os_calls`
// (monty-python `crates/monty-python/src/monty_cls.rs`). Filesystem ops
// matching a mount are handled inside Rust and resumed without returning
// to Elixir. Non-FS ops, unmounted FS ops, and all non-OsCall progress
// variants break out of the loop and surface to Elixir via
// `encode_run_progress`.

// Mount-aware variants perform host filesystem I/O (canonicalize, read,
// stat, ...) inside `drive_with_mounts`, which can block on slow storage
// (NFS/FUSE). Run them on a dirty *I/O* scheduler so a blocked FS call
// can't starve the small, CPU-count-sized DirtyCpu pool.
#[rustler::nif(schedule = "DirtyIo")]
fn start_with_mounts<'a>(
    env: Env<'a>,
    runner: ResourceArc<RunnerResource>,
    inputs: Vec<(String, Term<'a>)>,
    limits: Term<'a>,
    lease: ResourceArc<MountLease>,
) -> NifResult<Term<'a>> {
    let monty_run = runner.clone_runner();
    let monty_inputs = types::decode_inputs(env, inputs, runner.input_names())?;
    let resource_limits = types::decode_resource_limits(limits)?;
    let output_budget = OutputBudget::from_limits(&resource_limits);
    let tracker = ResourceTracker::new(resource_limits);
    let mut output = output_budget.collector();

    let initial = monty_run
        .start(monty_inputs, tracker, PrintWriter::Callback(&mut output))
        .map_err(error::monty_exception_to_rustler_error)?;

    let progress = drive_with_mounts(initial, &lease, &mut output)
        .map_err(error::monty_exception_to_rustler_error)?;

    encode_run_progress(env, progress, output)
}

#[rustler::nif(schedule = "DirtyIo")]
fn resume_with_mounts<'a>(
    env: Env<'a>,
    snapshot: ResourceArc<SnapshotResource>,
    result: Term<'a>,
    lease: ResourceArc<MountLease>,
) -> NifResult<Term<'a>> {
    let kind = snapshot.peek_kind().ok_or_else(snapshot_consumed_error)?;

    // Detect `:no_handler` from Elixir for OsCall snapshots — translate into
    // upstream's canonical `OsFunction::on_no_handler` exception before
    // resuming. For other snapshot kinds, decode the result *before* consuming
    // the snapshot so a malformed result leaves it intact for a retry.
    let no_handler = matches!(kind, SnapshotTag::OsCall) && is_no_handler_atom(result);
    let decoded = if no_handler {
        None
    } else {
        Some(decode_resume_result(env, kind, result)?)
    };
    let state = snapshot.take().ok_or_else(snapshot_consumed_error)?;
    let mut output = state.output_budget.collector();
    let print = PrintWriter::Callback(&mut output);

    let initial_progress = if no_handler {
        match state.snapshot {
            SnapshotKind::OsCall(call) => {
                let exc = call.function_call.on_no_handler();
                call.resume(ExtFunctionResult::Error(exc), print)
            }
            // Kind was peeked as OsCall and `take` is one-shot, so this is
            // unreachable; surface a clean error instead of panicking.
            _ => Err(MontyException::new(
                ExcType::RuntimeError,
                Some("snapshot kind changed".to_string()),
            )),
        }
    } else {
        match decoded {
            Some(decoded) => apply_resume(state.snapshot, decoded, print),
            None => Err(MontyException::new(
                ExcType::RuntimeError,
                Some("missing decoded callback result".to_string()),
            )),
        }
    }
    .map_err(error::monty_exception_to_rustler_error)?;

    let progress = drive_with_mounts(initial_progress, &lease, &mut output)
        .map_err(error::monty_exception_to_rustler_error)?;

    encode_run_progress(env, progress, output)
}

#[rustler::nif(schedule = "DirtyIo")]
fn resume_futures_with_mounts<'a>(
    env: Env<'a>,
    futures: ResourceArc<FutureSnapshotResource>,
    results: Vec<(u32, Term<'a>)>,
    lease: ResourceArc<MountLease>,
) -> NifResult<Term<'a>> {
    validate_future_result_ids(&futures, &results)?;

    // Decode all (attacker-controlled) results before consuming the snapshot.
    let external_results: Vec<(u32, ExtFunctionResult)> = results
        .into_iter()
        .map(|(id, term)| {
            let result = decode_external_result(env, term)?;
            Ok((id, result))
        })
        .collect::<NifResult<Vec<_>>>()?;

    let state = futures
        .take()
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("future snapshot already consumed")))?;

    let mut output = state.output_budget.collector();

    let initial_progress = state
        .snapshot
        .resume(external_results, PrintWriter::Callback(&mut output))
        .map_err(error::monty_exception_to_rustler_error)?;

    let progress = drive_with_mounts(initial_progress, &lease, &mut output)
        .map_err(error::monty_exception_to_rustler_error)?;

    encode_run_progress(env, progress, output)
}

fn is_no_handler_atom(term: Term<'_>) -> bool {
    if !term.is_atom() {
        return false;
    }
    matches!(term.atom_to_string().as_deref(), Ok("no_handler"))
}

/// Drives the os-call loop for mount-aware runs.
///
/// Locks the lease's table for the duration of this NIF call. Mount-handled
/// FS ops are resumed in-place without returning to Elixir. Returns when
/// any non-OsCall progress arrives, or when an OsCall doesn't match any
/// mount (non-FS or unmounted path).
fn drive_with_mounts(
    initial: RunProgress,
    lease: &MountLease,
    output: &mut OutputCollector,
) -> Result<RunProgress, MontyException> {
    let mut guard = match lease.table.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };

    let table = match guard.as_mut() {
        Some(t) => t,
        None => {
            // Lease was released between checkout and this NIF call —
            // shouldn't happen under correct Sandbox.run flow, but be
            // defensive.
            return Err(MontyException::new(
                ExcType::RuntimeError,
                Some("mount lease has been released".to_string()),
            ));
        }
    };

    let mut current = initial;
    loop {
        match current {
            RunProgress::OsCall(mut call) => {
                // `handle_os_call` takes the call by value so a covered
                // write's payload moves into overlay storage without a copy.
                // Swap in the cheapest unit variant as a placeholder; if the
                // table doesn't cover the call it hands it back and we restore
                // it before surfacing to Elixir.
                let function_call =
                    std::mem::replace(&mut call.function_call, OsFunctionCall::GetEnviron);
                match table.handle_os_call(function_call) {
                    MountCallOutcome::Handled(Ok(obj)) => {
                        let print = PrintWriter::Callback(output);
                        current = call.resume(ExtFunctionResult::Return(obj), print)?;
                    }
                    MountCallOutcome::Handled(Err(mount_err)) => {
                        let exc = mount_err.into_exception();
                        let print = PrintWriter::Callback(output);
                        current = call.resume(ExtFunctionResult::Error(exc), print)?;
                    }
                    MountCallOutcome::NotHandled(function_call) => {
                        // Non-FS op or unmounted FS path — surface to
                        // Elixir so a fallback handler can take it.
                        call.function_call = function_call;
                        return Ok(RunProgress::OsCall(call));
                    }
                }
            }
            other => return Ok(other),
        }
    }
}
