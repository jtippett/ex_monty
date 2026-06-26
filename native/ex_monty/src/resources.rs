use monty::{FunctionCall, LimitedTracker, MontyRun, NameLookup, OsCall, ResolveFutures};
use rustler::Resource;
use std::sync::{Mutex, MutexGuard};

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
    FunctionCall(FunctionCall<LimitedTracker>),
    OsCall(OsCall<LimitedTracker>),
    NameLookup(NameLookup<LimitedTracker>),
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
    snapshot: Mutex<Option<SnapshotKind>>,
}

impl SnapshotResource {
    pub fn new(snapshot: SnapshotKind) -> Self {
        Self {
            snapshot: Mutex::new(Some(snapshot)),
        }
    }

    /// Take the snapshot out, consuming it. Returns None if already taken.
    pub fn take(&self) -> Option<SnapshotKind> {
        lock_recover(&self.snapshot).take()
    }

    /// Inspect the snapshot's variant without consuming it. Returns None if
    /// already taken.
    pub fn peek_kind(&self) -> Option<SnapshotTag> {
        lock_recover(&self.snapshot).as_ref().map(|k| match k {
            SnapshotKind::FunctionCall(_) => SnapshotTag::FunctionCall,
            SnapshotKind::OsCall(_) => SnapshotTag::OsCall,
            SnapshotKind::NameLookup(_) => SnapshotTag::NameLookup,
        })
    }
}

#[rustler::resource_impl]
impl Resource for SnapshotResource {}

/// Wrapper around ResolveFutures<LimitedTracker>.
/// Uses Mutex<Option<...>> because resume consumes the snapshot.
pub struct FutureSnapshotResource {
    snapshot: Mutex<Option<ResolveFutures<LimitedTracker>>>,
}

impl FutureSnapshotResource {
    pub fn new(snapshot: ResolveFutures<LimitedTracker>) -> Self {
        Self {
            snapshot: Mutex::new(Some(snapshot)),
        }
    }

    /// Take the snapshot out, consuming it. Returns None if already taken.
    pub fn take(&self) -> Option<ResolveFutures<LimitedTracker>> {
        lock_recover(&self.snapshot).take()
    }

    /// Access the snapshot without consuming it (for pending_call_ids).
    pub fn with<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&ResolveFutures<LimitedTracker>) -> R,
    {
        let guard = lock_recover(&self.snapshot);
        guard.as_ref().map(f)
    }
}

#[rustler::resource_impl]
impl Resource for FutureSnapshotResource {}
