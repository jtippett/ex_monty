use monty::{MontyRun, RunProgress};
use monty_types::OsFunctionCall;
use rustler::Resource;
use std::sync::{Mutex, MutexGuard};

use crate::objects::RunState;

/// Lock a mutex, recovering the guard if a prior panic poisoned it.
///
/// The snapshot mutexes guard an `Option` taken at most once. A poisoned
/// lock (from an unwind in an earlier call) must not panic every later caller —
/// that would turn one transient failure into a permanent DoS of the
/// resource. The guarded value is still valid, so we recover the guard.
fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Wrapper around MontyRun for use as a Rustler resource.
///
/// A compiled `MontyRun` is not `Sync` (its intern tables use interior
/// mutability) and running one needs `&mut`, so the resource holds a pristine
/// copy behind a mutex and every execution works on its own clone. The lock is
/// held only for the clone.
pub struct RunnerResource {
    runner: Mutex<MontyRun>,
    input_names: Vec<String>,
}

impl RunnerResource {
    pub fn new(runner: MontyRun, input_names: Vec<String>) -> Self {
        Self {
            runner: Mutex::new(runner),
            input_names,
        }
    }

    pub fn clone_runner(&self) -> MontyRun {
        lock_recover(&self.runner).clone()
    }

    pub fn input_names(&self) -> &[String] {
        &self.input_names
    }
}

#[rustler::resource_impl]
impl Resource for RunnerResource {}

/// A suspended run: where it paused plus the state that travels with it.
pub struct Suspended {
    pub progress: RunProgress,
    pub state: RunState,
}

/// Lightweight tag identifying a snapshot's variant without consuming it.
/// Lets `resume` decode the (attacker-controlled) result for the right variant
/// *before* taking the snapshot, so a malformed result leaves it intact.
#[derive(Clone, Copy)]
pub enum SnapshotTag {
    FunctionCall,
    /// `accepts_future`: the call may be answered with `:pending`.
    OsCall {
        accepts_future: bool,
    },
    NameLookup,
}

impl SnapshotTag {
    /// The tag for a progress value a `SnapshotResource` may hold, or `None`
    /// for `ResolveFutures` / `Complete`.
    pub fn of(progress: &RunProgress) -> Option<Self> {
        match progress {
            RunProgress::FunctionCall(_) => Some(Self::FunctionCall),
            RunProgress::OsCall(call) => Some(Self::OsCall {
                accepts_future: OsFunctionCall::accepts_future(call.function_call.name()),
            }),
            RunProgress::NameLookup(_) => Some(Self::NameLookup),
            RunProgress::ResolveFutures(_) | RunProgress::Complete(_) => None,
        }
    }
}

/// A run suspended on a function call, OS call or name lookup.
/// Uses Mutex<Option<...>> because resume consumes the snapshot.
pub struct SnapshotResource {
    snapshot: Mutex<Option<Suspended>>,
}

impl SnapshotResource {
    /// `progress` must be a `FunctionCall`, `OsCall` or `NameLookup`.
    pub fn new(progress: RunProgress, state: RunState) -> Self {
        debug_assert!(SnapshotTag::of(&progress).is_some());
        Self {
            snapshot: Mutex::new(Some(Suspended { progress, state })),
        }
    }

    /// Take the snapshot out, consuming it. Returns None if already taken.
    pub fn take(&self) -> Option<Suspended> {
        lock_recover(&self.snapshot).take()
    }

    /// Inspect the snapshot's variant without consuming it. Returns None if
    /// already taken.
    pub fn peek_kind(&self) -> Option<SnapshotTag> {
        lock_recover(&self.snapshot)
            .as_ref()
            .and_then(|suspended| SnapshotTag::of(&suspended.progress))
    }
}

#[rustler::resource_impl]
impl Resource for SnapshotResource {}

/// A run suspended on unresolved futures.
/// Uses Mutex<Option<...>> because resume consumes the snapshot.
pub struct FutureSnapshotResource {
    snapshot: Mutex<Option<Suspended>>,
}

impl FutureSnapshotResource {
    /// `progress` must be a `ResolveFutures`.
    pub fn new(progress: RunProgress, state: RunState) -> Self {
        debug_assert!(matches!(progress, RunProgress::ResolveFutures(_)));
        Self {
            snapshot: Mutex::new(Some(Suspended { progress, state })),
        }
    }

    /// Take the snapshot out, consuming it. Returns None if already taken.
    pub fn take(&self) -> Option<Suspended> {
        lock_recover(&self.snapshot).take()
    }

    /// Access the suspended run without consuming it.
    pub fn with<R>(&self, f: impl FnOnce(&Suspended) -> R) -> Option<R> {
        lock_recover(&self.snapshot).as_ref().map(f)
    }

    /// The call ids still awaiting a result. None if already taken.
    pub fn pending_call_ids(&self) -> Option<Vec<u32>> {
        self.with(|suspended| match &suspended.progress {
            RunProgress::ResolveFutures(futures) => futures.pending_call_ids().to_vec(),
            _ => Vec::new(),
        })
    }
}

#[rustler::resource_impl]
impl Resource for FutureSnapshotResource {}
