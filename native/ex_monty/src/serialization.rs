use monty::{LimitedTracker, MontyRun};
use rustler::{Binary, Env, NifResult, OwnedBinary, ResourceArc};

use crate::resources::{FutureSnapshotResource, RunnerResource, SnapshotResource};

#[derive(serde::Serialize, serde::Deserialize)]
struct RunnerDump {
    runner: MontyRun,
    input_names: Vec<String>,
}

#[rustler::nif(schedule = "DirtyCpu")]
fn dump_runner(env: Env, runner: ResourceArc<RunnerResource>) -> NifResult<Binary> {
    let dump = RunnerDump {
        runner: runner.runner().clone(),
        input_names: runner.input_names().to_vec(),
    };

    let bytes = postcard::to_allocvec(&dump)
        .map_err(|e| rustler::Error::RaiseTerm(Box::new(format!("serialization error: {e}"))))?;
    let mut binary = OwnedBinary::new(bytes.len())
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("failed to allocate binary")))?;
    binary.as_mut_slice().copy_from_slice(&bytes);
    Ok(binary.release(env))
}

#[rustler::nif(schedule = "DirtyCpu")]
fn load_runner(binary: Binary) -> NifResult<ResourceArc<RunnerResource>> {
    let dump: RunnerDump = postcard::from_bytes(binary.as_slice())
        .map_err(|e| rustler::Error::RaiseTerm(Box::new(format!("deserialization error: {e}"))))?;
    Ok(ResourceArc::new(RunnerResource::new(
        dump.runner,
        dump.input_names,
    )))
}

#[rustler::nif(schedule = "DirtyCpu")]
fn dump_snapshot(env: Env, snapshot: ResourceArc<SnapshotResource>) -> NifResult<Binary> {
    let snap = snapshot
        .take()
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("snapshot already consumed")))?;

    let bytes = postcard::to_allocvec(&snap)
        .map_err(|e| rustler::Error::RaiseTerm(Box::new(format!("serialization error: {e}"))))?;

    let mut binary = OwnedBinary::new(bytes.len())
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("failed to allocate binary")))?;
    binary.as_mut_slice().copy_from_slice(&bytes);
    Ok(binary.release(env))
}

#[rustler::nif(schedule = "DirtyCpu")]
fn load_snapshot(binary: Binary) -> NifResult<ResourceArc<SnapshotResource>> {
    let snap: monty::Snapshot<LimitedTracker> = postcard::from_bytes(binary.as_slice())
        .map_err(|e| rustler::Error::RaiseTerm(Box::new(format!("deserialization error: {e}"))))?;
    Ok(ResourceArc::new(SnapshotResource::new(snap)))
}

#[rustler::nif(schedule = "DirtyCpu")]
fn dump_future_snapshot(
    env: Env,
    futures: ResourceArc<FutureSnapshotResource>,
) -> NifResult<Binary> {
    let snap = futures
        .take()
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("future snapshot already consumed")))?;

    let bytes = postcard::to_allocvec(&snap)
        .map_err(|e| rustler::Error::RaiseTerm(Box::new(format!("serialization error: {e}"))))?;

    let mut binary = OwnedBinary::new(bytes.len())
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("failed to allocate binary")))?;
    binary.as_mut_slice().copy_from_slice(&bytes);
    Ok(binary.release(env))
}

#[rustler::nif(schedule = "DirtyCpu")]
fn load_future_snapshot(binary: Binary) -> NifResult<ResourceArc<FutureSnapshotResource>> {
    let snap: monty::FutureSnapshot<LimitedTracker> = postcard::from_bytes(binary.as_slice())
        .map_err(|e| rustler::Error::RaiseTerm(Box::new(format!("deserialization error: {e}"))))?;
    Ok(ResourceArc::new(FutureSnapshotResource::new(snap)))
}
