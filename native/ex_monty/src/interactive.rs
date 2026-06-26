use monty::{
    ExtFunctionResult, LimitedTracker, MontyException, MontyObject, NameLookupResult, PrintWriter,
    RunProgress,
};
use rustler::types::atom::Atom;
use rustler::{Encoder, Env, NifResult, ResourceArc, Term};

use crate::error;
use crate::mounts::MountLease;
use crate::resources::{
    FutureSnapshotResource, RunnerResource, SnapshotKind, SnapshotResource, SnapshotTag,
};
use crate::types;

/// A resume result decoded for a specific snapshot variant, produced *before*
/// the snapshot is consumed so a malformed result can't destroy it.
enum DecodedResume {
    Ext(ExtFunctionResult),
    Name(NameLookupResult),
}

fn decode_resume_result<'a>(
    env: Env<'a>,
    kind: SnapshotTag,
    result: Term<'a>,
) -> NifResult<DecodedResume> {
    match kind {
        SnapshotTag::NameLookup => Ok(DecodedResume::Name(decode_name_lookup_result(env, result)?)),
        SnapshotTag::FunctionCall | SnapshotTag::OsCall => {
            Ok(DecodedResume::Ext(decode_external_result(env, result)?))
        }
    }
}

fn apply_resume(
    snap: SnapshotKind,
    decoded: DecodedResume,
    print: PrintWriter,
) -> Result<RunProgress<LimitedTracker>, MontyException> {
    match (snap, decoded) {
        (SnapshotKind::FunctionCall(call), DecodedResume::Ext(r)) => call.resume(r, print),
        (SnapshotKind::OsCall(call), DecodedResume::Ext(r)) => call.resume(r, print),
        (SnapshotKind::NameLookup(lookup), DecodedResume::Name(r)) => lookup.resume(r, print),
        // The snapshot variant is fixed once created and `take` is one-shot, so
        // the kind we peeked always matches the kind we took — these arms are
        // unreachable. Return a clean error rather than panic, just in case.
        _ => Err(MontyException::new(
            monty::ExcType::RuntimeError,
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
    let tracker = LimitedTracker::new(resource_limits);
    let mut output = String::new();

    let progress = monty_run
        .start(
            monty_inputs,
            tracker,
            PrintWriter::CollectString(&mut output),
        )
        .map_err(error::monty_exception_to_rustler_error)?;

    encode_run_progress(env, progress, &output)
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

    let snap = snapshot.take().ok_or_else(snapshot_consumed_error)?;

    let mut output = String::new();
    let print = PrintWriter::CollectString(&mut output);

    let progress =
        apply_resume(snap, decoded, print).map_err(error::monty_exception_to_rustler_error)?;

    encode_run_progress(env, progress, &output)
}

#[rustler::nif(schedule = "DirtyCpu")]
fn resume_futures<'a>(
    env: Env<'a>,
    futures: ResourceArc<FutureSnapshotResource>,
    results: Vec<(u32, Term<'a>)>,
) -> NifResult<Term<'a>> {
    // Decode all (attacker-controlled) results before consuming the snapshot,
    // so a malformed result leaves it intact for a retry.
    let external_results: Vec<(u32, ExtFunctionResult)> = results
        .into_iter()
        .map(|(id, term)| {
            let result = decode_external_result(env, term)?;
            Ok((id, result))
        })
        .collect::<NifResult<Vec<_>>>()?;

    let future_snap = futures
        .take()
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("future snapshot already consumed")))?;

    let mut output = String::new();

    let progress = future_snap
        .resume(external_results, PrintWriter::CollectString(&mut output))
        .map_err(error::monty_exception_to_rustler_error)?;

    encode_run_progress(env, progress, &output)
}

#[rustler::nif]
fn pending_call_ids(futures: ResourceArc<FutureSnapshotResource>) -> NifResult<Vec<u32>> {
    futures
        .with(|snap| snap.pending_call_ids().to_vec())
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("future snapshot already consumed")))
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn encode_run_progress<'a>(
    env: Env<'a>,
    progress: RunProgress<LimitedTracker>,
    output: &str,
) -> NifResult<Term<'a>> {
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
            let snapshot_ref =
                ResourceArc::new(SnapshotResource::new(SnapshotKind::FunctionCall(call)));
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
            let snapshot_ref = ResourceArc::new(SnapshotResource::new(SnapshotKind::OsCall(call)));
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
            let snapshot_ref =
                ResourceArc::new(SnapshotResource::new(SnapshotKind::NameLookup(lookup)));
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
            let futures_ref = ResourceArc::new(FutureSnapshotResource::new(future_snapshot));
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
    function: &monty::OsFunctionCall,
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

    if let Ok(elements) = get_tuple(term) {
        if elements.len() >= 2 {
            if let Ok(tag) = elements[0].atom_to_string() {
                match tag.as_str() {
                    "ok" => {
                        let obj = types::decode_monty_object(env, elements[1])?;
                        return Ok(ExtFunctionResult::Return(obj));
                    }
                    "error" => {
                        if elements.len() == 3 {
                            let type_str: String = elements[1].decode().unwrap_or_else(|_| {
                                elements[1]
                                    .atom_to_string()
                                    .unwrap_or_else(|_| "runtime_error".to_string())
                            });
                            let msg: String = elements[2]
                                .decode()
                                .unwrap_or_else(|_| "unknown error".to_string());
                            let exc_type = parse_exc_type(&type_str);
                            let exc = MontyException::new(exc_type, Some(msg));
                            return Ok(ExtFunctionResult::Error(exc));
                        } else {
                            let msg: String = elements[1]
                                .decode()
                                .unwrap_or_else(|_| "unknown error".to_string());
                            let exc = MontyException::new(monty::ExcType::RuntimeError, Some(msg));
                            return Ok(ExtFunctionResult::Error(exc));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // If it's just a value, treat as return
    let obj = types::decode_monty_object(env, term)?;
    Ok(ExtFunctionResult::Return(obj))
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

    // Raw value → Value(obj)
    let obj = types::decode_monty_object(env, term)?;
    Ok(NameLookupResult::Value(obj))
}

fn parse_exc_type(s: &str) -> monty::ExcType {
    // Try parsing both snake_case and PascalCase
    use std::str::FromStr;
    if let Ok(t) = monty::ExcType::from_str(s) {
        return t;
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
    monty::ExcType::from_str(&pascal).unwrap_or(monty::ExcType::RuntimeError)
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
    let tracker = LimitedTracker::new(resource_limits);
    let mut output = String::new();

    let initial = monty_run
        .start(
            monty_inputs,
            tracker,
            PrintWriter::CollectString(&mut output),
        )
        .map_err(error::monty_exception_to_rustler_error)?;

    let progress = drive_with_mounts(initial, &lease, &mut output)
        .map_err(error::monty_exception_to_rustler_error)?;

    encode_run_progress(env, progress, &output)
}

#[rustler::nif(schedule = "DirtyIo")]
fn resume_with_mounts<'a>(
    env: Env<'a>,
    snapshot: ResourceArc<SnapshotResource>,
    result: Term<'a>,
    lease: ResourceArc<MountLease>,
) -> NifResult<Term<'a>> {
    let kind = snapshot.peek_kind().ok_or_else(snapshot_consumed_error)?;

    let mut output = String::new();
    let print = PrintWriter::CollectString(&mut output);

    // Detect `:no_handler` from Elixir for OsCall snapshots — translate into
    // upstream's canonical `OsFunction::on_no_handler` exception before
    // resuming. For other snapshot kinds, decode the result *before* consuming
    // the snapshot so a malformed result leaves it intact for a retry.
    let initial_progress = if matches!(kind, SnapshotTag::OsCall) && is_no_handler_atom(result) {
        match snapshot.take().ok_or_else(snapshot_consumed_error)? {
            SnapshotKind::OsCall(call) => {
                let exc = call.function_call.on_no_handler();
                call.resume(ExtFunctionResult::Error(exc), print)
            }
            // Kind was peeked as OsCall and `take` is one-shot, so this is
            // unreachable; surface a clean error instead of panicking.
            _ => Err(MontyException::new(
                monty::ExcType::RuntimeError,
                Some("snapshot kind changed".to_string()),
            )),
        }
    } else {
        let decoded = decode_resume_result(env, kind, result)?;
        let snap = snapshot.take().ok_or_else(snapshot_consumed_error)?;
        apply_resume(snap, decoded, print)
    }
    .map_err(error::monty_exception_to_rustler_error)?;

    let progress = drive_with_mounts(initial_progress, &lease, &mut output)
        .map_err(error::monty_exception_to_rustler_error)?;

    encode_run_progress(env, progress, &output)
}

#[rustler::nif(schedule = "DirtyIo")]
fn resume_futures_with_mounts<'a>(
    env: Env<'a>,
    futures: ResourceArc<FutureSnapshotResource>,
    results: Vec<(u32, Term<'a>)>,
    lease: ResourceArc<MountLease>,
) -> NifResult<Term<'a>> {
    // Decode all (attacker-controlled) results before consuming the snapshot.
    let external_results: Vec<(u32, ExtFunctionResult)> = results
        .into_iter()
        .map(|(id, term)| {
            let result = decode_external_result(env, term)?;
            Ok((id, result))
        })
        .collect::<NifResult<Vec<_>>>()?;

    let future_snap = futures
        .take()
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("future snapshot already consumed")))?;

    let mut output = String::new();

    let initial_progress = future_snap
        .resume(external_results, PrintWriter::CollectString(&mut output))
        .map_err(error::monty_exception_to_rustler_error)?;

    let progress = drive_with_mounts(initial_progress, &lease, &mut output)
        .map_err(error::monty_exception_to_rustler_error)?;

    encode_run_progress(env, progress, &output)
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
    initial: RunProgress<LimitedTracker>,
    lease: &MountLease,
    output: &mut String,
) -> Result<RunProgress<LimitedTracker>, MontyException> {
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
                monty::ExcType::RuntimeError,
                Some("mount lease has been released".to_string()),
            ));
        }
    };

    let mut current = initial;
    loop {
        match current {
            RunProgress::OsCall(call) => {
                match table.handle_os_call(&call.function_call) {
                    Some(Ok(obj)) => {
                        let print = PrintWriter::CollectString(output);
                        current = call.resume(ExtFunctionResult::Return(obj), print)?;
                    }
                    Some(Err(mount_err)) => {
                        let exc = mount_err.into_exception();
                        let print = PrintWriter::CollectString(output);
                        current = call.resume(ExtFunctionResult::Error(exc), print)?;
                    }
                    None => {
                        // Non-FS op or unmounted FS path — surface to
                        // Elixir so a fallback handler can take it.
                        return Ok(RunProgress::OsCall(call));
                    }
                }
            }
            other => return Ok(other),
        }
    }
}
