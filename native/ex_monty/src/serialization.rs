use monty::{LimitedTracker, MontyRun};
use rustler::{Binary, Env, NifResult, OwnedBinary, ResourceArc};

use crate::resources::{FutureSnapshotResource, RunnerResource, SnapshotResource};

#[rustler::nif]
fn dump_runner(env: Env, runner: ResourceArc<RunnerResource>) -> NifResult<Binary> {
    let bytes = runner
        .runner()
        .dump()
        .map_err(|e| rustler::Error::RaiseTerm(Box::new(format!("serialization error: {e}"))))?;
    let mut binary = OwnedBinary::new(bytes.len())
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("failed to allocate binary")))?;
    binary.as_mut_slice().copy_from_slice(&bytes);
    Ok(binary.release(env))
}

#[rustler::nif]
fn load_runner(binary: Binary) -> NifResult<ResourceArc<RunnerResource>> {
    let runner = MontyRun::load(binary.as_slice())
        .map_err(|e| rustler::Error::RaiseTerm(Box::new(format!("deserialization error: {e}"))))?;
    Ok(ResourceArc::new(RunnerResource::new(runner)))
}

#[rustler::nif]
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

#[rustler::nif]
fn load_snapshot(binary: Binary) -> NifResult<ResourceArc<SnapshotResource>> {
    let snap: monty::Snapshot<LimitedTracker> = postcard::from_bytes(binary.as_slice())
        .map_err(|e| rustler::Error::RaiseTerm(Box::new(format!("deserialization error: {e}"))))?;
    Ok(ResourceArc::new(SnapshotResource::new(snap)))
}

#[rustler::nif]
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

#[rustler::nif]
fn load_future_snapshot(binary: Binary) -> NifResult<ResourceArc<FutureSnapshotResource>> {
    let snap: monty::FutureSnapshot<LimitedTracker> = postcard::from_bytes(binary.as_slice())
        .map_err(|e| rustler::Error::RaiseTerm(Box::new(format!("deserialization error: {e}"))))?;
    Ok(ResourceArc::new(FutureSnapshotResource::new(snap)))
}
