use monty::{FunctionCall, LimitedTracker, MontyRun, NameLookup, OsCall, ResolveFutures};
use rustler::Resource;
use std::sync::Mutex;

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
        self.snapshot.lock().unwrap().take()
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
        self.snapshot.lock().unwrap().take()
    }

    /// Access the snapshot without consuming it (for pending_call_ids).
    pub fn with<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&ResolveFutures<LimitedTracker>) -> R,
    {
        let guard = self.snapshot.lock().unwrap();
        guard.as_ref().map(f)
    }
}

#[rustler::resource_impl]
impl Resource for FutureSnapshotResource {}
