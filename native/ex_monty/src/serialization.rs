use monty::{Dump, MontyRun, RunProgress, Session, SessionRef};
use rustler::{Binary, Env, NifResult, OwnedBinary, ResourceArc};
use serde::Deserialize;

use crate::objects::RunState;
use crate::resources::{FutureSnapshotResource, RunnerResource, SnapshotResource, Suspended};

// Version 3 (monty v1): the interpreter state of a snapshot is upstream's own
// versioned session dump (CBOR), preceded by the state ExMonty keeps alongside
// it. Runner dumps gained a header too. Older payloads (v2 = monty v0.0.21
// postcard, and header-less runner dumps) are wire-incompatible and fail with
// a clean "unsupported format" error.
//
// Layout of a snapshot:
//   header (5 bytes) | u32-BE length of RunState | RunState (CBOR) | monty dump
const RUNNER_HEADER: &[u8] = b"EXMR\x03";
const SNAPSHOT_HEADER: &[u8] = b"EXMS\x03";
const FUTURE_SNAPSHOT_HEADER: &[u8] = b"EXMF\x03";

#[derive(serde::Serialize)]
struct RunnerDumpRef<'a> {
    runner: &'a MontyRun,
    input_names: &'a [String],
}

#[derive(serde::Deserialize)]
struct RunnerDump {
    runner: MontyRun,
    input_names: Vec<String>,
}

fn raise(message: String) -> rustler::Error {
    rustler::Error::RaiseTerm(Box::new(message))
}

fn serialization_error(e: impl std::fmt::Display) -> rustler::Error {
    raise(format!("serialization error: {e}"))
}

fn format_error(kind: &str) -> rustler::Error {
    raise(format!(
        "unsupported or corrupt {kind} serialization format"
    ))
}

#[rustler::nif(schedule = "DirtyCpu")]
fn dump_runner(env: Env, runner: ResourceArc<RunnerResource>) -> NifResult<Binary> {
    let monty_run = runner.clone_runner();
    let dump = RunnerDumpRef {
        runner: &monty_run,
        input_names: runner.input_names(),
    };
    let bytes = minicbor_serde::to_vec(&dump).map_err(serialization_error)?;
    encode_parts(env, &[RUNNER_HEADER, &bytes])
}

#[rustler::nif(schedule = "DirtyCpu")]
fn load_runner(binary: Binary) -> NifResult<ResourceArc<RunnerResource>> {
    let bytes = strip_header(binary.as_slice(), RUNNER_HEADER, "runner")?;
    let dump: RunnerDump = decode_exact(bytes, "runner")?;
    Ok(ResourceArc::new(RunnerResource::new(
        // The policy is part of the dump, but it is what keeps the clock and
        // sleeps host-controlled, so it is re-asserted rather than trusted.
        dump.runner.with_os_policy(crate::sandbox_os_policy()),
        dump.input_names,
    )))
}

#[rustler::nif(schedule = "DirtyCpu")]
fn dump_snapshot(env: Env, snapshot: ResourceArc<SnapshotResource>) -> NifResult<Binary> {
    let suspended = snapshot
        .take()
        .ok_or_else(|| raise("snapshot already consumed".to_string()))?;
    dump_suspended(env, SNAPSHOT_HEADER, &suspended)
}

#[rustler::nif(schedule = "DirtyCpu")]
fn load_snapshot(binary: Binary) -> NifResult<ResourceArc<SnapshotResource>> {
    let kind = "snapshot";
    let suspended = load_suspended(binary.as_slice(), SNAPSHOT_HEADER, kind)?;
    match suspended.progress {
        RunProgress::FunctionCall(_) | RunProgress::OsCall(_) | RunProgress::NameLookup(_) => Ok(
            ResourceArc::new(SnapshotResource::new(suspended.progress, suspended.state)),
        ),
        RunProgress::ResolveFutures(_) | RunProgress::Complete(_) => Err(format_error(kind)),
    }
}

#[rustler::nif(schedule = "DirtyCpu")]
fn dump_future_snapshot(
    env: Env,
    futures: ResourceArc<FutureSnapshotResource>,
) -> NifResult<Binary> {
    let suspended = futures
        .take()
        .ok_or_else(|| raise("future snapshot already consumed".to_string()))?;
    dump_suspended(env, FUTURE_SNAPSHOT_HEADER, &suspended)
}

#[rustler::nif(schedule = "DirtyCpu")]
fn load_future_snapshot(binary: Binary) -> NifResult<ResourceArc<FutureSnapshotResource>> {
    let kind = "future snapshot";
    let suspended = load_suspended(binary.as_slice(), FUTURE_SNAPSHOT_HEADER, kind)?;
    match suspended.progress {
        RunProgress::ResolveFutures(_) => Ok(ResourceArc::new(FutureSnapshotResource::new(
            suspended.progress,
            suspended.state,
        ))),
        _ => Err(format_error(kind)),
    }
}

fn dump_suspended<'a>(env: Env<'a>, header: &[u8], suspended: &Suspended) -> NifResult<Binary<'a>> {
    let state = minicbor_serde::to_vec(&suspended.state).map_err(serialization_error)?;
    let state_len = u32::try_from(state.len())
        .map_err(|_| raise("serialized snapshot is too large".to_string()))?
        .to_be_bytes();
    let session = monty::dump("", None, &[], SessionRef::Running(&suspended.progress))
        .map_err(serialization_error)?;
    encode_parts(env, &[header, &state_len, &state, &session])
}

fn load_suspended(bytes: &[u8], header: &[u8], kind: &str) -> NifResult<Suspended> {
    let bytes = strip_header(bytes, header, kind)?;
    let (len_bytes, rest) = bytes
        .split_first_chunk::<4>()
        .ok_or_else(|| format_error(kind))?;
    let state_len = u32::from_be_bytes(*len_bytes) as usize;
    if state_len > rest.len() {
        return Err(format_error(kind));
    }
    let (state_bytes, session_bytes) = rest.split_at(state_len);
    let state: RunState = decode_exact(state_bytes, kind)?;
    // `Dump::load` checks upstream's magic and dump version, and rejects
    // trailing bytes.
    let dump =
        Dump::load(session_bytes).map_err(|e| raise(format!("deserialization error: {e}")))?;
    match dump.state {
        Session::Running(progress) => Ok(Suspended {
            progress: *progress,
            state,
        }),
        _ => Err(format_error(kind)),
    }
}

fn encode_parts<'a>(env: Env<'a>, parts: &[&[u8]]) -> NifResult<Binary<'a>> {
    let total_len = parts
        .iter()
        .try_fold(0usize, |total, part| total.checked_add(part.len()))
        .ok_or_else(|| raise("serialized snapshot is too large".to_string()))?;
    let mut binary = OwnedBinary::new(total_len)
        .ok_or_else(|| raise("failed to allocate binary".to_string()))?;
    let mut out = binary.as_mut_slice();
    for part in parts {
        let (head, tail) = out.split_at_mut(part.len());
        head.copy_from_slice(part);
        out = tail;
    }
    Ok(binary.release(env))
}

/// Deserialize a CBOR payload and reject any trailing bytes, so a valid dump
/// with appended garbage does not load as if it were intact.
fn decode_exact<'de, T: Deserialize<'de>>(bytes: &'de [u8], kind: &str) -> NifResult<T> {
    let mut deserializer = minicbor_serde::Deserializer::new(bytes);
    let value = T::deserialize(&mut deserializer)
        .map_err(|e| raise(format!("deserialization error: {e}")))?;
    if deserializer.into_decoder().position() != bytes.len() {
        return Err(format_error(kind));
    }
    Ok(value)
}

fn strip_header<'a>(bytes: &'a [u8], header: &[u8], kind: &str) -> NifResult<&'a [u8]> {
    bytes.strip_prefix(header).ok_or_else(|| format_error(kind))
}
