use monty::{FunctionCall, MontyRun, NameLookup, OsCall, ResolveFutures};
use rustler::Resource;
use std::sync::{Mutex, MutexGuard};

use crate::output::OutputBudget;

/// Lock a snapshot mutex, recovering the guard if a prior panic poisoned it.
///
/// These mutexes guard an `Option<Snapshot>` taken at most once. A poisoned
/// lock (from an unwind in an earlier call) must not panic every later caller —
/// that would turn one transient failure into a permanent DoS of the snapshot
/// resource. The `Option` is still valid, so we recover the guard.
fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Wrapper around MontyRun for use as a Rustler resource.
/// MontyRun is Clone, so we can share it safely.
pub struct RunnerResource {
    runner: MontyRun,
    input_names: Vec<String>,
}

impl RunnerResource {
    pub fn new(runner: MontyRun, input_names: Vec<String>) -> Self {
        Self {
            runner,
            input_names,
        }
    }

    pub fn runner(&self) -> &MontyRun {
        &self.runner
    }

    pub fn clone_runner(&self) -> MontyRun {
        self.runner.clone()
    }

    pub fn input_names(&self) -> &[String] {
        &self.input_names
    }
}

#[rustler::resource_impl]
impl Resource for RunnerResource {}

/// Enum wrapping the resumable RunProgress variants that take ExtFunctionResult
/// or NameLookupResult.
#[derive(serde::Serialize, serde::Deserialize)]
pub enum SnapshotKind {
    FunctionCall(FunctionCall),
    OsCall(OsCall),
    NameLookup(NameLookup),
}

/// Serializable one-shot snapshot plus the print-output budget accumulated by
/// the same run. Keeping these together prevents dump/load or resume from
/// resetting a configured memory ceiling.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SnapshotState {
    pub snapshot: SnapshotKind,
    pub output_budget: OutputBudget,
}

/// Lightweight tag identifying a snapshot's variant without consuming it.
/// Lets `resume` decode the (attacker-controlled) result for the right variant
/// *before* taking the snapshot, so a malformed result leaves it intact.
#[derive(Clone, Copy)]
pub enum SnapshotTag {
    FunctionCall,
    OsCall,
    NameLookup,
}

/// Wrapper around resumable snapshot variants.
/// Uses Mutex<Option<...>> because resume consumes the snapshot.
pub struct SnapshotResource {
    snapshot: Mutex<Option<SnapshotState>>,
}

impl SnapshotResource {
    pub fn new(snapshot: SnapshotKind, output_budget: OutputBudget) -> Self {
        Self {
            snapshot: Mutex::new(Some(SnapshotState {
                snapshot,
                output_budget,
            })),
        }
    }

    /// Take the snapshot out, consuming it. Returns None if already taken.
    pub fn take(&self) -> Option<SnapshotState> {
        lock_recover(&self.snapshot).take()
    }

    /// Inspect the snapshot's variant without consuming it. Returns None if
    /// already taken.
    pub fn peek_kind(&self) -> Option<SnapshotTag> {
        lock_recover(&self.snapshot)
            .as_ref()
            .map(|state| match &state.snapshot {
                SnapshotKind::FunctionCall(_) => SnapshotTag::FunctionCall,
                SnapshotKind::OsCall(_) => SnapshotTag::OsCall,
                SnapshotKind::NameLookup(_) => SnapshotTag::NameLookup,
            })
    }
}

#[rustler::resource_impl]
impl Resource for SnapshotResource {}

/// Wrapper around ResolveFutures.
/// Uses Mutex<Option<...>> because resume consumes the snapshot.
pub struct FutureSnapshotResource {
    snapshot: Mutex<Option<FutureSnapshotState>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct FutureSnapshotState {
    pub snapshot: ResolveFutures,
    pub output_budget: OutputBudget,
}

impl FutureSnapshotResource {
    pub fn new(snapshot: ResolveFutures, output_budget: OutputBudget) -> Self {
        Self {
            snapshot: Mutex::new(Some(FutureSnapshotState {
                snapshot,
                output_budget,
            })),
        }
    }

    /// Take the snapshot out, consuming it. Returns None if already taken.
    pub fn take(&self) -> Option<FutureSnapshotState> {
        lock_recover(&self.snapshot).take()
    }

    /// Access the snapshot without consuming it (for pending_call_ids).
    pub fn with<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&ResolveFutures) -> R,
    {
        let guard = lock_recover(&self.snapshot);
        guard.as_ref().map(|state| f(&state.snapshot))
    }
}

#[rustler::resource_impl]
impl Resource for FutureSnapshotResource {}
