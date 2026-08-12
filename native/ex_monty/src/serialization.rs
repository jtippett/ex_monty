use monty::MontyRun;
use rustler::{Binary, Env, NifResult, OwnedBinary, ResourceArc};

use crate::resources::{
    FutureSnapshotResource, FutureSnapshotState, RunnerResource, SnapshotResource, SnapshotState,
};

// Version 2: monty v0.0.21 restructured the serialized form of MontyRun and
// the resumable snapshot types, so v1 payloads are wire-incompatible. Bumping
// the header makes old dumps fail with a clean "unsupported format" error
// instead of a garbled postcard decode.
const SNAPSHOT_HEADER: &[u8] = b"EXMS\x02";
const FUTURE_SNAPSHOT_HEADER: &[u8] = b"EXMF\x02";

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

    encode_versioned(env, SNAPSHOT_HEADER, &bytes)
}

fn encode_versioned<'a>(env: Env<'a>, header: &[u8], bytes: &[u8]) -> NifResult<Binary<'a>> {
    let total_len = header
        .len()
        .checked_add(bytes.len())
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("serialized snapshot is too large")))?;
    let mut binary = OwnedBinary::new(total_len)
        .ok_or_else(|| rustler::Error::RaiseTerm(Box::new("failed to allocate binary")))?;
    let (header_out, bytes_out) = binary.as_mut_slice().split_at_mut(header.len());
    header_out.copy_from_slice(header);
    bytes_out.copy_from_slice(bytes);
    Ok(binary.release(env))
}

#[rustler::nif(schedule = "DirtyCpu")]
fn load_snapshot(binary: Binary) -> NifResult<ResourceArc<SnapshotResource>> {
    let bytes = strip_header(binary.as_slice(), SNAPSHOT_HEADER, "snapshot")?;
    let snap: SnapshotState = decode_exact(bytes, "snapshot")?;
    Ok(ResourceArc::new(SnapshotResource::new(
        snap.snapshot,
        snap.output_budget,
    )))
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

    encode_versioned(env, FUTURE_SNAPSHOT_HEADER, &bytes)
}

#[rustler::nif(schedule = "DirtyCpu")]
fn load_future_snapshot(binary: Binary) -> NifResult<ResourceArc<FutureSnapshotResource>> {
    let bytes = strip_header(binary.as_slice(), FUTURE_SNAPSHOT_HEADER, "future snapshot")?;
    let snap: FutureSnapshotState = decode_exact(bytes, "future snapshot")?;
    Ok(ResourceArc::new(FutureSnapshotResource::new(
        snap.snapshot,
        snap.output_budget,
    )))
}

/// Deserialize a postcard payload and reject any trailing bytes. `from_bytes`
/// silently ignores unused input, so a valid dump with appended garbage would
/// otherwise load as if it were intact.
fn decode_exact<T: serde::de::DeserializeOwned>(bytes: &[u8], kind: &str) -> NifResult<T> {
    let (value, rest) = postcard::take_from_bytes::<T>(bytes)
        .map_err(|e| rustler::Error::RaiseTerm(Box::new(format!("deserialization error: {e}"))))?;
    if !rest.is_empty() {
        return Err(rustler::Error::RaiseTerm(Box::new(format!(
            "unsupported or corrupt {kind} serialization format"
        ))));
    }
    Ok(value)
}

fn strip_header<'a>(bytes: &'a [u8], header: &[u8], kind: &str) -> NifResult<&'a [u8]> {
    bytes.strip_prefix(header).ok_or_else(|| {
        rustler::Error::RaiseTerm(Box::new(format!(
            "unsupported or corrupt {kind} serialization format"
        )))
    })
}
