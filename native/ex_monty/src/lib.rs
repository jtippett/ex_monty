mod error;
mod interactive;
mod mounts;
mod output;
mod resources;
mod serialization;
mod types;

use monty_types::{CompileOptions, PrintWriter, ResourceTracker};
use resources::RunnerResource;
use rustler::{Encoder, Env, NifResult, ResourceArc, Term};

#[rustler::nif(schedule = "DirtyCpu")]
fn compile(
    code: String,
    script_name: String,
    input_names: Vec<String>,
) -> NifResult<ResourceArc<RunnerResource>> {
    let input_names_for_resource = input_names.clone();
    let runner = monty::MontyRun::new(code, &script_name, input_names, CompileOptions::default())
        .map_err(error::monty_exception_to_rustler_error)?;
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
    let runner_ref = runner.runner();
    let monty_inputs = types::decode_inputs(env, inputs, runner.input_names())?;
    let resource_limits = types::decode_resource_limits(limits)?;
    let output_budget = output::OutputBudget::from_limits(&resource_limits);
    let tracker = ResourceTracker::new(resource_limits);
    let mut output = output_budget.collector();

    let result = runner_ref
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
