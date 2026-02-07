use monty::{FutureSnapshot, LimitedTracker, MontyRun, Snapshot};
use rustler::Resource;
use std::sync::Mutex;

/// Wrapper around MontyRun for use as a Rustler resource.
/// MontyRun is Clone, so we can share it safely.
pub struct RunnerResource {
    runner: MontyRun,
}

impl RunnerResource {
    pub fn new(runner: MontyRun) -> Self {
        Self { runner }
    }

    pub fn runner(&self) -> &MontyRun {
        &self.runner
    }

    pub fn clone_runner(&self) -> MontyRun {
        self.runner.clone()
    }
}

#[rustler::resource_impl]
impl Resource for RunnerResource {}

/// Wrapper around Snapshot<LimitedTracker>.
/// Uses Mutex<Option<...>> because Snapshot::run consumes self.
pub struct SnapshotResource {
    snapshot: Mutex<Option<Snapshot<LimitedTracker>>>,
}

impl SnapshotResource {
    pub fn new(snapshot: Snapshot<LimitedTracker>) -> Self {
        Self {
            snapshot: Mutex::new(Some(snapshot)),
        }
    }

    /// Take the snapshot out, consuming it. Returns None if already taken.
    pub fn take(&self) -> Option<Snapshot<LimitedTracker>> {
        self.snapshot.lock().unwrap().take()
    }
}

#[rustler::resource_impl]
impl Resource for SnapshotResource {}

/// Wrapper around FutureSnapshot<LimitedTracker>.
/// Uses Mutex<Option<...>> because FutureSnapshot::resume consumes self.
pub struct FutureSnapshotResource {
    snapshot: Mutex<Option<FutureSnapshot<LimitedTracker>>>,
}

impl FutureSnapshotResource {
    pub fn new(snapshot: FutureSnapshot<LimitedTracker>) -> Self {
        Self {
            snapshot: Mutex::new(Some(snapshot)),
        }
    }

    /// Take the snapshot out, consuming it. Returns None if already taken.
    pub fn take(&self) -> Option<FutureSnapshot<LimitedTracker>> {
        self.snapshot.lock().unwrap().take()
    }

    /// Access the snapshot without consuming it (for pending_call_ids).
    pub fn with<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&FutureSnapshot<LimitedTracker>) -> R,
    {
        let guard = self.snapshot.lock().unwrap();
        guard.as_ref().map(f)
    }
}

#[rustler::resource_impl]
impl Resource for FutureSnapshotResource {}
