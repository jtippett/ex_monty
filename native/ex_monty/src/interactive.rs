use monty::{FunctionCall, RunProgress};
use monty_fs::MountCallOutcome;
use monty_types::{
    ExcType, ExtFunctionResult, MontyException, NameLookupResult, OsFunctionCall, PrintWriter,
    ResourceTracker,
};
use rustler::types::atom::Atom;
use rustler::types::tuple::{get_tuple, make_tuple};
use rustler::{Encoder, Env, NifResult, ResourceArc, Term};
use std::collections::HashSet;

use crate::error;
use crate::mounts::MountLease;
use crate::objects::{HostObjects, RunState};
use crate::output::{OutputBudget, OutputCollector};
use crate::resources::{
    FutureSnapshotResource, RunnerResource, SnapshotResource, SnapshotTag, Suspended,
};
use crate::types::{self, HostObject};

// ── NIFs ─────────────────────────────────────────────────────────────────────
//
// Each entry point exists twice: plain, and mount-aware. The mount-aware
// variants answer filesystem OS calls that fall under a mount inside Rust
// (see `drive`) and so perform host filesystem I/O (open, read, stat, ...),
// which can block on slow storage (NFS/FUSE). They run on a dirty *I/O*
// scheduler so a blocked FS call can't starve the small, CPU-count-sized
// DirtyCpu pool.

#[rustler::nif(schedule = "DirtyCpu")]
fn start<'a>(
    env: Env<'a>,
    runner: ResourceArc<RunnerResource>,
    inputs: Vec<(String, Term<'a>)>,
    limits: Term<'a>,
) -> NifResult<Term<'a>> {
    start_run(env, &runner, inputs, limits, None)
}

#[rustler::nif(schedule = "DirtyIo")]
fn start_with_mounts<'a>(
    env: Env<'a>,
    runner: ResourceArc<RunnerResource>,
    inputs: Vec<(String, Term<'a>)>,
    limits: Term<'a>,
    lease: ResourceArc<MountLease>,
) -> NifResult<Term<'a>> {
    start_run(env, &runner, inputs, limits, Some(&lease))
}

#[rustler::nif(schedule = "DirtyCpu")]
fn resume<'a>(
    env: Env<'a>,
    snapshot: ResourceArc<SnapshotResource>,
    result: Term<'a>,
) -> NifResult<Term<'a>> {
    resume_run(env, &snapshot, result, None)
}

#[rustler::nif(schedule = "DirtyIo")]
fn resume_with_mounts<'a>(
    env: Env<'a>,
    snapshot: ResourceArc<SnapshotResource>,
    result: Term<'a>,
    lease: ResourceArc<MountLease>,
) -> NifResult<Term<'a>> {
    resume_run(env, &snapshot, result, Some(&lease))
}

#[rustler::nif(schedule = "DirtyCpu")]
fn resume_futures<'a>(
    env: Env<'a>,
    futures: ResourceArc<FutureSnapshotResource>,
    results: Vec<(u32, Term<'a>)>,
) -> NifResult<Term<'a>> {
    resume_futures_run(env, &futures, results, None)
}

#[rustler::nif(schedule = "DirtyIo")]
fn resume_futures_with_mounts<'a>(
    env: Env<'a>,
    futures: ResourceArc<FutureSnapshotResource>,
    results: Vec<(u32, Term<'a>)>,
    lease: ResourceArc<MountLease>,
) -> NifResult<Term<'a>> {
    resume_futures_run(env, &futures, results, Some(&lease))
}

#[rustler::nif(schedule = "DirtyCpu")]
fn pending_call_ids(futures: ResourceArc<FutureSnapshotResource>) -> NifResult<Vec<u32>> {
    futures
        .pending_call_ids()
        .ok_or_else(future_snapshot_consumed_error)
}

// ── Start / resume ───────────────────────────────────────────────────────────

fn start_run<'a>(
    env: Env<'a>,
    runner: &RunnerResource,
    inputs: Vec<(String, Term<'a>)>,
    limits: Term<'a>,
    lease: Option<&MountLease>,
) -> NifResult<Term<'a>> {
    let monty_run = runner.clone_runner();
    let (monty_inputs, host_objects) = types::decode_inputs(env, inputs, runner.input_names())?;
    let resource_limits = types::decode_resource_limits(limits)?;
    let mut state = RunState::new(OutputBudget::from_limits(&resource_limits));
    state.objects.extend(host_objects);
    let tracker = ResourceTracker::new(resource_limits);
    let mut output = state.output_budget.collector();

    let progress = monty_run
        .start(monty_inputs, tracker, PrintWriter::Callback(&mut output))
        .and_then(|progress| drive(progress, lease, &mut output, &state.objects))
        .map_err(error::monty_exception_to_rustler_error)?;

    encode_run_progress(env, progress, output, state)
}

fn resume_run<'a>(
    env: Env<'a>,
    snapshot: &SnapshotResource,
    result: Term<'a>,
    lease: Option<&MountLease>,
) -> NifResult<Term<'a>> {
    // Decode the (attacker-controlled) result *before* consuming the snapshot,
    // so a malformed result returns an error and leaves the snapshot intact for
    // a retry instead of silently destroying it.
    let kind = snapshot.peek_kind().ok_or_else(snapshot_consumed_error)?;
    let (decoded, host_objects) = decode_resume_result(env, kind, result)?;

    let Suspended {
        progress,
        mut state,
    } = snapshot.take().ok_or_else(snapshot_consumed_error)?;
    state.objects.extend(host_objects);
    let mut output = state.output_budget.collector();

    let progress = apply_resume(progress, decoded, PrintWriter::Callback(&mut output))
        .and_then(|progress| drive(progress, lease, &mut output, &state.objects))
        .map_err(error::monty_exception_to_rustler_error)?;

    encode_run_progress(env, progress, output, state)
}

fn resume_futures_run<'a>(
    env: Env<'a>,
    futures: &FutureSnapshotResource,
    results: Vec<(u32, Term<'a>)>,
    lease: Option<&MountLease>,
) -> NifResult<Term<'a>> {
    validate_future_result_ids(futures, &results)?;

    // Decode all (attacker-controlled) results before consuming the snapshot,
    // so a malformed result leaves it intact for a retry.
    let mut host_objects: Vec<HostObject> = Vec::new();
    let mut external_results: Vec<(u32, ExtFunctionResult)> = Vec::with_capacity(results.len());
    for (id, term) in results {
        external_results.push((id, decode_external_result(env, term, &mut host_objects)?));
    }

    let Suspended {
        progress,
        mut state,
    } = futures.take().ok_or_else(future_snapshot_consumed_error)?;
    state.objects.extend(host_objects);
    let mut output = state.output_budget.collector();

    let progress = match progress {
        RunProgress::ResolveFutures(pending) => {
            pending.resume(external_results, PrintWriter::Callback(&mut output))
        }
        // A future snapshot only ever holds `ResolveFutures`.
        _ => Err(kind_mismatch()),
    }
    .and_then(|progress| drive(progress, lease, &mut output, &state.objects))
    .map_err(error::monty_exception_to_rustler_error)?;

    encode_run_progress(env, progress, output, state)
}

/// A resume result decoded for a specific snapshot variant, produced *before*
/// the snapshot is consumed so a malformed result can't destroy it.
enum DecodedResume {
    Ext(ExtFunctionResult),
    Name(NameLookupResult),
    /// `:pending` — the call's result will arrive later as a future.
    Pending,
    /// `:no_handler` — the host has nothing to answer this OS call with.
    NoHandler,
}

fn decode_resume_result<'a>(
    env: Env<'a>,
    kind: SnapshotTag,
    result: Term<'a>,
) -> NifResult<(DecodedResume, Vec<HostObject>)> {
    let mut host_objects = Vec::new();
    let decoded = match kind {
        SnapshotTag::NameLookup => {
            DecodedResume::Name(decode_name_lookup_result(env, result, &mut host_objects)?)
        }
        SnapshotTag::FunctionCall if is_atom_named(result, "pending") => DecodedResume::Pending,
        SnapshotTag::OsCall {
            accepts_future: true,
        } if is_atom_named(result, "pending") => DecodedResume::Pending,
        SnapshotTag::OsCall { .. } if is_atom_named(result, "no_handler") => {
            DecodedResume::NoHandler
        }
        SnapshotTag::FunctionCall | SnapshotTag::OsCall { .. } => {
            DecodedResume::Ext(decode_external_result(env, result, &mut host_objects)?)
        }
    };
    Ok((decoded, host_objects))
}

fn apply_resume(
    progress: RunProgress,
    decoded: DecodedResume,
    print: PrintWriter,
) -> Result<RunProgress, MontyException> {
    match (progress, decoded) {
        (RunProgress::FunctionCall(call), DecodedResume::Ext(r)) => call.resume(r, print),
        (RunProgress::FunctionCall(call), DecodedResume::Pending) => call.resume_pending(print),
        (RunProgress::OsCall(call), DecodedResume::Ext(r)) => call.resume(r, print),
        (RunProgress::OsCall(call), DecodedResume::Pending) => {
            let call_id = call.call_id;
            call.resume(ExtFunctionResult::Future(call_id), print)
        }
        (RunProgress::OsCall(call), DecodedResume::NoHandler) => {
            // Upstream's canonical "the host does not provide this" answer:
            // `ModuleNotFoundError` for an import, the matching `OSError` /
            // `NotImplementedError` for everything else.
            let result = call.function_call.no_handler_result();
            call.resume(result, print)
        }
        (RunProgress::NameLookup(lookup), DecodedResume::Name(r)) => lookup.resume(r, print),
        // The snapshot variant is fixed once created and `take` is one-shot, so
        // the kind we peeked always matches the kind we took — these arms are
        // unreachable. Return a clean error rather than panic, just in case.
        _ => Err(kind_mismatch()),
    }
}

fn kind_mismatch() -> MontyException {
    MontyException::new(
        ExcType::RuntimeError,
        Some("snapshot kind/result mismatch".to_string()),
    )
}

fn snapshot_consumed_error() -> rustler::Error {
    rustler::Error::RaiseTerm(Box::new("snapshot already consumed"))
}

fn future_snapshot_consumed_error() -> rustler::Error {
    rustler::Error::RaiseTerm(Box::new("future snapshot already consumed"))
}

// ── The in-NIF part of the host loop ─────────────────────────────────────────

/// Advances a run past every suspension that is answered inside Rust, and
/// returns the first one the Elixir host has to see.
///
/// Answered here:
///
/// * Filesystem OS calls covered by a mount (when `lease` is given). This
///   mirrors upstream's `drive_run_progress_through_os_calls`
///   (`crates/monty-python/src/monty_cls.rs`). The lease's table stays locked
///   for the duration of the NIF call. Non-FS and unmounted calls surface.
/// * A read of an attribute a host object does not have. The sandbox already
///   holds every field the host sent, so anything else does not exist:
///   `AttributeError`, as before monty grew lazy host attributes.
/// * A method call on a host object this run has no record of, and a call of
///   the object itself.
fn drive(
    initial: RunProgress,
    lease: Option<&MountLease>,
    output: &mut OutputCollector,
    objects: &HostObjects,
) -> Result<RunProgress, MontyException> {
    let mut guard = lease.map(|lease| match lease.table.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    });
    let mut table = match guard.as_mut() {
        Some(guard) => match (**guard).as_mut() {
            Some(table) => Some(table),
            // Lease was released between checkout and this NIF call —
            // shouldn't happen under correct Sandbox.run flow, but be
            // defensive.
            None => {
                return Err(MontyException::new(
                    ExcType::RuntimeError,
                    Some("mount lease has been released".to_string()),
                ))
            }
        },
        None => None,
    };

    let mut current = initial;
    loop {
        match current {
            RunProgress::OsCall(mut call) => {
                let Some(table) = table.as_mut() else {
                    return Ok(RunProgress::OsCall(call));
                };
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
            RunProgress::NameLookup(lookup) if lookup.object_id().is_some() => {
                let print = PrintWriter::Callback(output);
                current = lookup.resume(NameLookupResult::Undefined, print)?;
            }
            RunProgress::FunctionCall(call) => match reject_method_call(&call, objects) {
                Some(exc) => {
                    let print = PrintWriter::Callback(output);
                    current = call.resume(ExtFunctionResult::Error(exc), print)?;
                }
                None => return Ok(RunProgress::FunctionCall(call)),
            },
            other => return Ok(other),
        }
    }
}

/// The exception to raise in the sandbox for a method call the host cannot be
/// asked about, or `None` when the call should surface.
fn reject_method_call(call: &FunctionCall, objects: &HostObjects) -> Option<MontyException> {
    let object_id = call.object_id.as_ref()?;
    let Some(object) = objects.get(object_id) else {
        return Some(MontyException::new(
            ExcType::RuntimeError,
            Some(format!(
                "cannot call '{}' on a host object this run no longer tracks",
                call.function_name
            )),
        ));
    };
    if call.function_name == "__call__" {
        let class_name = HostObjects::class_name(object).unwrap_or("object");
        return Some(MontyException::new(
            ExcType::TypeError,
            Some(format!("'{class_name}' object is not callable")),
        ));
    }
    None
}

// ── Encoding progress for Elixir ─────────────────────────────────────────────

fn encode_run_progress<'a>(
    env: Env<'a>,
    progress: RunProgress,
    output: OutputCollector,
    mut state: RunState,
) -> NifResult<Term<'a>> {
    let (output, output_budget) = output.finish();
    state.output_budget = output_budget;
    let output_term = output.encode(env);
    let atom = |name: &str| Atom::from_str(env, name).unwrap().encode(env);

    match progress {
        RunProgress::FunctionCall(call) => {
            let (mut args, kwargs) = types::encode_call_args(env, &call.args)?;
            // A method call on a host object names its receiver by id only.
            // ExMonty's API passes the receiver itself as the first argument.
            let tag = match &call.object_id {
                Some(object_id) => {
                    let receiver = state.objects.get(object_id).ok_or_else(|| {
                        rustler::Error::Term(Box::new("method call on an unknown host object"))
                    })?;
                    args.insert(0, types::encode_monty_object(env, receiver)?);
                    atom("method_call")
                }
                None => atom("function_call"),
            };
            let call_term = encode_call_struct(
                env,
                "Elixir.ExMonty.FunctionCall",
                ("name", call.function_name.encode(env)),
                args,
                kwargs,
                call.call_id,
            );
            let snapshot_ref = ResourceArc::new(SnapshotResource::new(
                RunProgress::FunctionCall(call),
                state,
            ));
            Ok(make_tuple(
                env,
                &[tag, call_term, snapshot_ref.encode(env), output_term],
            ))
        }
        RunProgress::OsCall(call) => {
            // `to_args` consumes the call value, so clone it for the Elixir-facing
            // view and keep the original in the snapshot for resume/no-handler.
            let call_args = call.function_call.clone().to_args();
            let (args, kwargs) = types::encode_call_args(env, &call_args)?;
            let call_term = encode_call_struct(
                env,
                "Elixir.ExMonty.OsCall",
                (
                    "function",
                    types::encode_os_function(env, &call.function_call),
                ),
                args,
                kwargs,
                call.call_id,
            );
            let snapshot_ref =
                ResourceArc::new(SnapshotResource::new(RunProgress::OsCall(call), state));
            Ok(make_tuple(
                env,
                &[
                    atom("os_call"),
                    call_term,
                    snapshot_ref.encode(env),
                    output_term,
                ],
            ))
        }
        RunProgress::NameLookup(lookup) => {
            let name_term = lookup.name.encode(env);
            let snapshot_ref = ResourceArc::new(SnapshotResource::new(
                RunProgress::NameLookup(lookup),
                state,
            ));
            Ok(make_tuple(
                env,
                &[
                    atom("name_lookup"),
                    name_term,
                    snapshot_ref.encode(env),
                    output_term,
                ],
            ))
        }
        RunProgress::ResolveFutures(pending) => {
            let futures_ref = ResourceArc::new(FutureSnapshotResource::new(
                RunProgress::ResolveFutures(pending),
                state,
            ));
            Ok(make_tuple(
                env,
                &[
                    atom("resolve_futures"),
                    futures_ref.encode(env),
                    output_term,
                ],
            ))
        }
        RunProgress::Complete(value) => {
            let value_term = types::encode_monty_object(env, &value)?;
            Ok(make_tuple(
                env,
                &[atom("complete"), value_term, output_term],
            ))
        }
    }
}

/// Builds an `%ExMonty.FunctionCall{}` / `%ExMonty.OsCall{}`; `subject` is the
/// struct's own first field (`name` / `function`).
fn encode_call_struct<'a>(
    env: Env<'a>,
    struct_name: &str,
    subject: (&str, Term<'a>),
    args: Vec<Term<'a>>,
    kwargs: Term<'a>,
    call_id: u32,
) -> Term<'a> {
    let atom = |name: &str| Atom::from_str(env, name).unwrap().encode(env);
    rustler::types::map::map_new(env)
        .map_put(atom("__struct__"), atom(struct_name))
        .unwrap()
        .map_put(atom(subject.0), subject.1)
        .unwrap()
        .map_put(atom("args"), args.encode(env))
        .unwrap()
        .map_put(atom("kwargs"), kwargs)
        .unwrap()
        .map_put(atom("call_id"), call_id.encode(env))
        .unwrap()
}

// ── Decoding host results ────────────────────────────────────────────────────

fn decode_exception(exc_type: Term<'_>, message: Term<'_>) -> NifResult<MontyException> {
    let type_str = decode_atom_or_string(exc_type)?;
    let msg: String = message.decode().map_err(|_| invalid_callback_result())?;
    let exc_type = parse_exc_type(&type_str)?;
    Ok(MontyException::new(exc_type, Some(msg)))
}

fn decode_external_result<'a>(
    env: Env<'a>,
    term: Term<'a>,
    host_objects: &mut Vec<HostObject>,
) -> NifResult<ExtFunctionResult> {
    let elements = get_tuple(term).map_err(|_| invalid_callback_result())?;
    let tag = elements
        .first()
        .ok_or_else(invalid_callback_result)?
        .atom_to_string()
        .map_err(|_| invalid_callback_result())?;

    match (tag.as_str(), elements.as_slice()) {
        ("ok", [_, value]) => {
            let decoded = types::decode_monty_object(env, *value)?;
            host_objects.extend(decoded.host_objects);
            Ok(ExtFunctionResult::Return(decoded.object))
        }
        ("error", [_, exc_type, message]) => Ok(ExtFunctionResult::Error(decode_exception(
            *exc_type, *message,
        )?)),
        _ => Err(invalid_callback_result()),
    }
}

fn decode_name_lookup_result<'a>(
    env: Env<'a>,
    term: Term<'a>,
    host_objects: &mut Vec<HostObject>,
) -> NifResult<NameLookupResult> {
    // :undefined → Undefined
    if is_atom_named(term, "undefined") {
        return Ok(NameLookupResult::Undefined);
    }

    if let Ok(elements) = get_tuple(term) {
        if let Some(Ok(tag)) = elements.first().map(|t| t.atom_to_string()) {
            match (tag.as_str(), elements.as_slice()) {
                // {:ok, value} → Value(obj)
                ("ok", [_, value]) => {
                    let decoded = types::decode_monty_object(env, *value)?;
                    host_objects.extend(decoded.host_objects);
                    return Ok(NameLookupResult::Value(decoded.object));
                }
                // {:error, type, message} → raise in the sandbox
                ("error", [_, exc_type, message]) => {
                    return Ok(NameLookupResult::Error(decode_exception(
                        *exc_type, *message,
                    )?));
                }
                _ => {}
            }
        }
    }

    Err(rustler::Error::Term(Box::new(
        "name lookup result must be :undefined, {:ok, value} or {:error, exception_type, message}",
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

fn is_atom_named(term: Term<'_>, name: &str) -> bool {
    term.is_atom() && matches!(term.atom_to_string().as_deref(), Ok(s) if s == name)
}

fn validate_future_result_ids(
    futures: &FutureSnapshotResource,
    results: &[(u32, Term<'_>)],
) -> NifResult<()> {
    let pending = futures
        .pending_call_ids()
        .ok_or_else(future_snapshot_consumed_error)?;
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
