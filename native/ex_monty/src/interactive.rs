use monty::{
    CollectStringPrint, ExternalResult, LimitedTracker, MontyException, MontyObject, RunProgress,
};
use rustler::types::atom::Atom;
use rustler::{Encoder, Env, NifResult, ResourceArc, Term};

use crate::error;
use crate::resources::{FutureSnapshotResource, RunnerResource, SnapshotResource};
use crate::types;

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
    let mut print = CollectStringPrint::new();

    let progress = monty_run
        .start(monty_inputs, tracker, &mut print)
        .map_err(|e| error::monty_exception_to_rustler_error(e))?;

    let output = print.into_output();
    encode_run_progress(env, progress, &output)
}

#[rustler::nif(schedule = "DirtyCpu")]
fn resume<'a>(
    env: Env<'a>,
    snapshot: ResourceArc<SnapshotResource>,
    result: Term<'a>,
) -> NifResult<Term<'a>> {
    let snap = snapshot
        .take()
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("snapshot already consumed")))?;

    let external_result = decode_external_result(env, result)?;
    let mut print = CollectStringPrint::new();

    let progress = snap
        .run(external_result, &mut print)
        .map_err(|e| error::monty_exception_to_rustler_error(e))?;

    let output = print.into_output();
    encode_run_progress(env, progress, &output)
}

#[rustler::nif(schedule = "DirtyCpu")]
fn resume_futures<'a>(
    env: Env<'a>,
    futures: ResourceArc<FutureSnapshotResource>,
    results: Vec<(u32, Term<'a>)>,
) -> NifResult<Term<'a>> {
    let future_snap = futures
        .take()
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("future snapshot already consumed")))?;

    let external_results: Vec<(u32, ExternalResult)> = results
        .into_iter()
        .map(|(id, term)| {
            let result = decode_external_result(env, term)?;
            Ok((id, result))
        })
        .collect::<NifResult<Vec<_>>>()?;

    let mut print = CollectStringPrint::new();

    let progress = future_snap
        .resume(external_results, &mut print)
        .map_err(|e| error::monty_exception_to_rustler_error(e))?;

    let output = print.into_output();
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
        RunProgress::FunctionCall {
            function_name,
            args,
            kwargs,
            call_id,
            state,
        } => {
            let tag = Atom::from_str(env, "function_call").unwrap();
            let call = encode_function_call(env, &function_name, &args, &kwargs, call_id);
            let snapshot_ref = ResourceArc::new(SnapshotResource::new(state));
            Ok(rustler::types::tuple::make_tuple(
                env,
                &[tag.encode(env), call, snapshot_ref.encode(env), output_term],
            ))
        }
        RunProgress::OsCall {
            function,
            args,
            kwargs,
            call_id,
            state,
        } => {
            let tag = Atom::from_str(env, "os_call").unwrap();
            let call = encode_os_call(env, &function, &args, &kwargs, call_id);
            let snapshot_ref = ResourceArc::new(SnapshotResource::new(state));
            Ok(rustler::types::tuple::make_tuple(
                env,
                &[tag.encode(env), call, snapshot_ref.encode(env), output_term],
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
            let value_term = types::encode_monty_object(env, &value);
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
) -> Term<'a> {
    let struct_atom = Atom::from_str(env, "Elixir.ExMonty.FunctionCall").unwrap();

    let args_term: Vec<Term> = args
        .iter()
        .map(|a| types::encode_monty_object(env, a))
        .collect();
    let kwargs_term = encode_kwargs(env, kwargs);

    rustler::types::map::map_new(env)
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
        .unwrap()
}

fn encode_os_call<'a>(
    env: Env<'a>,
    function: &monty::OsFunction,
    args: &[MontyObject],
    kwargs: &[(MontyObject, MontyObject)],
    call_id: u32,
) -> Term<'a> {
    let struct_atom = Atom::from_str(env, "Elixir.ExMonty.OsCall").unwrap();

    let func_term = types::encode_os_function(env, function);
    let args_term: Vec<Term> = args
        .iter()
        .map(|a| types::encode_monty_object(env, a))
        .collect();
    let kwargs_term = encode_kwargs(env, kwargs);

    rustler::types::map::map_new(env)
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
        .unwrap()
}

fn encode_kwargs<'a>(env: Env<'a>, kwargs: &[(MontyObject, MontyObject)]) -> Term<'a> {
    let mut map = rustler::types::map::map_new(env);
    for (k, v) in kwargs {
        // kwargs keys are typically strings in Python
        let key = types::encode_monty_object(env, k);
        let val = types::encode_monty_object(env, v);
        map = map.map_put(key, val).unwrap();
    }
    map
}

fn decode_external_result<'a>(env: Env<'a>, term: Term<'a>) -> NifResult<ExternalResult> {
    use rustler::types::tuple::get_tuple;

    if let Ok(elements) = get_tuple(term) {
        if elements.len() >= 2 {
            if let Ok(tag) = elements[0].atom_to_string() {
                match tag.as_str() {
                    "ok" => {
                        let obj = types::decode_monty_object(env, elements[1])?;
                        return Ok(ExternalResult::Return(obj));
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
                            return Ok(ExternalResult::Error(exc));
                        } else {
                            let msg: String = elements[1]
                                .decode()
                                .unwrap_or_else(|_| "unknown error".to_string());
                            let exc = MontyException::new(monty::ExcType::RuntimeError, Some(msg));
                            return Ok(ExternalResult::Error(exc));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // If it's just a value, treat as return
    let obj = types::decode_monty_object(env, term)?;
    Ok(ExternalResult::Return(obj))
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
