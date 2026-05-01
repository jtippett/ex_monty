//! Mount-table resources and lease lifecycle.
//!
//! See `proposals/MOUNT_TABLE.md` for the design rationale. Key shape:
//!
//! - `MountResource` holds a `Mutex<Option<MountTable>>` plus a separate
//!   `Mutex<Vec<MountDescriptor>>` side-channel. The descriptors are
//!   readable while the table is leased, so `count/1` / `list/1` work
//!   during an active run.
//! - `MountLease` is a separate Rustler resource that owns the table for
//!   the duration of a run. Released either explicitly via `release/1`
//!   (idempotent) or via Drop when the term becomes unreachable.

use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use monty::fs::{MountMode, MountTable, OverlayState};
use rustler::{Atom, Encoder, Env, Resource, ResourceArc, Term};

mod atoms {
    rustler::atoms! {
        ok,
        error,
        // Error reasons
        mount_in_use,
        invalid_virtual_path,
        invalid_mode,
        host_path_not_found,
        host_path_not_directory,
        host_path_canonicalize_failed,
        already_mounted,
        // Mode tags
        read_only,
        read_write,
        overlay,
        // Other
        unlimited,
        nil,
    }
}

/// Lightweight, always-readable metadata about a configured mount.
///
/// Mirrors the data inside `monty::fs::Mount` but doesn't depend on the
/// table being present, so `count/1` and `list/1` work while leased.
#[derive(Clone)]
struct MountDescriptor {
    virtual_path: String,
    host_path: String,
    mode: ModeKind,
    write_bytes_limit: Option<u64>,
}

#[derive(Clone, Copy)]
enum ModeKind {
    ReadOnly,
    ReadWrite,
    Overlay,
}

impl ModeKind {
    fn as_atom(self) -> Atom {
        match self {
            Self::ReadOnly => atoms::read_only(),
            Self::ReadWrite => atoms::read_write(),
            Self::Overlay => atoms::overlay(),
        }
    }

    fn to_mount_mode(self) -> MountMode {
        match self {
            Self::ReadOnly => MountMode::ReadOnly,
            Self::ReadWrite => MountMode::ReadWrite,
            Self::Overlay => MountMode::OverlayMemory(OverlayState::new()),
        }
    }
}

fn parse_mode_atom(atom: Atom, env: Env) -> Option<ModeKind> {
    if atom == atoms::read_only() {
        Some(ModeKind::ReadOnly)
    } else if atom == atoms::read_write() {
        Some(ModeKind::ReadWrite)
    } else if atom == atoms::overlay() {
        Some(ModeKind::Overlay)
    } else {
        let _ = env;
        None
    }
}

/// The durable mount-table resource backing `%ExMonty.Mount{}`.
pub struct MountResource {
    /// The live `MountTable`. `None` while a lease is active.
    pub(crate) table: Mutex<Option<MountTable>>,
    /// Lightweight metadata, populated in lockstep with `table`. Survives
    /// while the table is leased so `count/1` / `list/1` can read it.
    descriptors: Mutex<Vec<MountDescriptor>>,
}

#[rustler::resource_impl]
impl Resource for MountResource {}

impl MountResource {
    fn new() -> Self {
        Self {
            table: Mutex::new(Some(MountTable::new())),
            descriptors: Mutex::new(Vec::new()),
        }
    }
}

/// The per-run lease backing `%ExMonty.Mount.Lease{}`.
pub struct MountLease {
    /// Source resource, kept alive for the lease's duration.
    pub(crate) source: ResourceArc<MountResource>,
    /// The owned table for the lease's lifetime. `None` after release.
    pub(crate) table: Mutex<Option<MountTable>>,
    /// CAS flag so concurrent `release/1` calls (or release + Drop)
    /// serialise to a single put-back.
    released: AtomicBool,
}

#[rustler::resource_impl]
impl Resource for MountLease {}

impl Drop for MountLease {
    fn drop(&mut self) {
        // Same path as explicit release; the CAS handles double-release
        // and "release then Drop" cases identically.
        release_lease_inner(self);
    }
}

/// Idempotent put-back. Single CAS winner moves the table back into the
/// source resource; subsequent callers short-circuit.
fn release_lease_inner(lease: &MountLease) {
    if lease.released.swap(true, Ordering::AcqRel) {
        return;
    }

    let table = match lease.table.lock() {
        Ok(mut guard) => guard.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    };

    if let Some(table) = table {
        if let Ok(mut source_slot) = lease.source.table.lock() {
            *source_slot = Some(table);
        }
        // If the source mutex is poisoned, intentionally drop the table.
        // The mount resource will then be permanently leased (visible as
        // `:mount_in_use` for any subsequent op) until GC'd. Better than
        // panicking inside Drop.
    }
}

// ── NIFs ─────────────────────────────────────────────────────────────────

#[rustler::nif]
pub fn mounts_new() -> ResourceArc<MountResource> {
    ResourceArc::new(MountResource::new())
}

#[rustler::nif]
pub fn mounts_count(resource: ResourceArc<MountResource>) -> usize {
    resource.descriptors.lock().unwrap().len()
}

#[rustler::nif]
pub fn mounts_list<'a>(env: Env<'a>, resource: ResourceArc<MountResource>) -> Term<'a> {
    let descriptors = resource.descriptors.lock().unwrap();
    let entries: Vec<Term<'a>> = descriptors
        .iter()
        .map(|d| encode_list_entry(env, d))
        .collect();
    entries.encode(env)
}

fn encode_list_entry<'a>(env: Env<'a>, d: &MountDescriptor) -> Term<'a> {
    let map = rustler::types::map::map_new(env);
    let map = map
        .map_put(
            Atom::from_str(env, "virtual").unwrap().encode(env),
            d.virtual_path.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "host").unwrap().encode(env),
            d.host_path.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "mode").unwrap().encode(env),
            d.mode.as_atom().encode(env),
        )
        .unwrap();

    let limit_term = match d.write_bytes_limit {
        Some(n) => n.encode(env),
        None => atoms::unlimited().encode(env),
    };
    map.map_put(
        Atom::from_str(env, "write_bytes_limit").unwrap().encode(env),
        limit_term,
    )
    .unwrap()
}

#[rustler::nif]
pub fn mounts_add<'a>(
    env: Env<'a>,
    resource: ResourceArc<MountResource>,
    virtual_path: String,
    host_path: String,
    mode_atom: Atom,
    write_bytes_limit: Option<u64>,
) -> Term<'a> {
    let mode = match parse_mode_atom(mode_atom, env) {
        Some(m) => m,
        None => return error_atom(env, atoms::invalid_mode()),
    };

    // Acquire table first; bail early if leased.
    let mut table_guard = resource.table.lock().unwrap();
    let table = match table_guard.as_mut() {
        Some(t) => t,
        None => return error_atom(env, atoms::mount_in_use()),
    };

    // Now lock descriptors. Order: table → descriptors. count/1 only
    // touches descriptors so this never deadlocks with it.
    let mut descriptors = resource.descriptors.lock().unwrap();

    if descriptors
        .iter()
        .any(|d| d.virtual_path == virtual_path)
    {
        return error_tuple(env, atoms::already_mounted(), virtual_path.encode(env));
    }

    match table.mount(&virtual_path, &host_path, mode.to_mount_mode(), write_bytes_limit) {
        Ok(()) => {
            descriptors.push(MountDescriptor {
                virtual_path,
                host_path,
                mode,
                write_bytes_limit,
            });
            atoms::ok().encode(env)
        }
        Err(mount_err) => error_atom(env, classify_mount_error(&mount_err)),
    }
}

fn classify_mount_error(err: &monty::fs::MountError) -> Atom {
    use monty::fs::MountError;
    match err {
        MountError::InvalidMount(msg) => {
            // Upstream's InvalidMount covers virtual-path validation, host
            // existence, host-is-directory, and canonicalisation. Tease
            // them apart by message keyword; if we miss, fall through to
            // a generic invalid-virtual-path bucket — consistent with
            // upstream's own bucketing.
            let m = msg.to_lowercase();
            if m.contains("does not exist") || m.contains("not found") {
                atoms::host_path_not_found()
            } else if m.contains("not a directory") || m.contains("is a file") {
                atoms::host_path_not_directory()
            } else if m.contains("canonical") {
                atoms::host_path_canonicalize_failed()
            } else {
                atoms::invalid_virtual_path()
            }
        }
        // Other MountError variants (NoMountPoint, PathEscape, ReadOnly,
        // CrossMountRename, Io, InvalidUtf8, WriteLimitExceeded) don't
        // surface from MountTable::mount; they come from handle_os_call.
        _ => atoms::invalid_virtual_path(),
    }
}

#[rustler::nif]
pub fn mounts_checkout<'a>(
    env: Env<'a>,
    resource: ResourceArc<MountResource>,
) -> Term<'a> {
    let mut guard = resource.table.lock().unwrap();
    match guard.take() {
        Some(table) => {
            let lease = MountLease {
                source: resource.clone(),
                table: Mutex::new(Some(table)),
                released: AtomicBool::new(false),
            };
            ok_tuple(env, ResourceArc::new(lease).encode(env))
        }
        None => error_atom(env, atoms::mount_in_use()),
    }
}

#[rustler::nif]
pub fn mounts_release(lease: ResourceArc<MountLease>) -> Atom {
    release_lease_inner(&lease);
    atoms::ok()
}

// ── Encoding helpers ─────────────────────────────────────────────────────

fn ok_tuple<'a>(env: Env<'a>, value: Term<'a>) -> Term<'a> {
    rustler::types::tuple::make_tuple(env, &[atoms::ok().encode(env), value])
}

fn error_atom<'a>(env: Env<'a>, reason: Atom) -> Term<'a> {
    rustler::types::tuple::make_tuple(env, &[atoms::error().encode(env), reason.encode(env)])
}

fn error_tuple<'a>(env: Env<'a>, tag: Atom, payload: Term<'a>) -> Term<'a> {
    let inner = rustler::types::tuple::make_tuple(env, &[tag.encode(env), payload]);
    rustler::types::tuple::make_tuple(env, &[atoms::error().encode(env), inner])
}
