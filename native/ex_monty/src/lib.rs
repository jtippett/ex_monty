mod error;
mod interactive;
mod mounts;
mod objects;
mod output;
mod resources;
mod serialization;
mod types;

use monty_types::{
    CompileOptions, DateTimeSource, OsPolicy, PrintWriter, ResourceTracker, SleepMode,
};
use resources::RunnerResource;
use rustler::{Encoder, Env, NifResult, ResourceArc, Term};

/// The OS policy every ExMonty sandbox runs under.
///
/// Upstream's defaults read the system clock and perform `time.sleep()` by
/// blocking in-process. Inside a NIF that would let untrusted code observe the
/// host's wall clock without the host being asked, and park a dirty scheduler
/// thread for the length of the sleep. Both are routed to the host as OS calls
/// instead (`:date_today` / `:datetime_now` / `:time`, `:sleep` /
/// `:async_sleep`), exactly like every other OS access.
pub(crate) fn sandbox_os_policy() -> OsPolicy {
    OsPolicy {
        datetime: DateTimeSource::CallHost,
        sleep: SleepMode::CallHost,
        ..OsPolicy::default()
    }
}

#[rustler::nif(schedule = "DirtyCpu")]
fn compile(
    code: String,
    script_name: String,
    input_names: Vec<String>,
) -> NifResult<ResourceArc<RunnerResource>> {
    let input_names_for_resource = input_names.clone();
    let runner = monty::MontyRun::new(code, &script_name, input_names, CompileOptions::default())
        .map_err(error::monty_exception_to_rustler_error)?
        .with_os_policy(sandbox_os_policy());
    Ok(ResourceArc::new(RunnerResource::new(
        runner,
        input_names_for_resource,
    )))
}

#[rustler::nif(schedule = "DirtyCpu")]
fn run<'a>(
    env: Env<'a>,
    runner: ResourceArc<RunnerResource>,
    inputs: Vec<(String, Term<'a>)>,
    limits: Term<'a>,
) -> NifResult<Term<'a>> {
    let mut monty_run = runner.clone_runner();
    // A one-shot run cannot suspend, so there is nobody to answer a method
    // call on a host object and the decoded host objects are not kept.
    let (monty_inputs, _host_objects) = types::decode_inputs(env, inputs, runner.input_names())?;
    let resource_limits = types::decode_resource_limits(limits)?;
    let output_budget = output::OutputBudget::from_limits(&resource_limits);
    let tracker = ResourceTracker::new(resource_limits);
    let mut output = output_budget.collector();

    let result = monty_run
        .run(monty_inputs, tracker, PrintWriter::Callback(&mut output))
        .map_err(error::monty_exception_to_rustler_error)?;

    let (output, _budget) = output.finish();
    let result_term = types::encode_monty_object(env, &result)?;
    let output_term = output.encode(env);
    Ok(rustler::types::tuple::make_tuple(
        env,
        &[result_term, output_term],
    ))
}

rustler::init!("Elixir.ExMonty.Native");
