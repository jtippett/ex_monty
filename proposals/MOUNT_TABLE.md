# Proposal: `ExMonty.Mount` — host filesystem mounts in the sandbox

**Status:** Final draft, revision 4 (ready to implement)
**Author:** James Tippett
**Target:** ExMonty v0.4 (post-v0.0.17 update)

> **Revision 4 changes (final review pass):**
> - `count/1` is **permissive under lease.** Mount count and lightweight
>   descriptors live in a separate `Mutex<Vec<MountDescriptor>>` alongside
>   the leased table, so `count/1` works without contending with the
>   active run. `list/1` still requires the table (it needs live
>   `write_bytes_used`) and so still errors under lease. (§2, §4, §5.2)
> - **`:invalid_mode` added to `add_error`** — bad mode atom is an obvious
>   user-facing failure. (§2)
> - **`release/1` is explicitly idempotent.** Calling it twice returns
>   `:ok`. Lease `Drop` after explicit release is a no-op. Implemented
>   with an `AtomicBool` released flag plus CAS. (§5.2)
> - **Lease Drop wording softened.** Killed-process release happens "when
>   the lease resource becomes unreachable to the GC," not
>   "deterministically when the BEAM process dies." Prompt release is
>   guaranteed only by `try/after`. (§4)
> - **Concurrent lease use tolerated** with mutex + idempotent state
>   transitions. Doesn't assume an opaque term stays in one process. (§5.2)
>
> **Revision 3 changes (preserved for context):**
> - **Explicit run lease.** `Mount.checkout/1` takes the table out for the
>   entire logical run; NIFs operate on the lease, never the bare mount.
>   Closes the interleave gap during paused Elixir callbacks. (§2, §4)
> - **`:no_handler` is computed Rust-side.** Elixir dispatch returns
>   `:no_handler`; the Rust loop calls `OsFunction::on_no_handler`. (§5.1)
> - **`:mount_in_use` added to `add_error` type and `list/1`'s contract.**
> - **Toned-down panic claims.** Drop guard for unwind safety, not panic
>   recovery. (§5.1)
> - **`overlay_bytes` and inline shorthand deferred.** (§7)
>
> **Revision 2 changes (preserved for context):**
> - Mounts are a **separate `mounts:` option**, composing with `:os`. (§3)
> - **Mount-aware loop happens in Rust** via new NIFs mirroring upstream's
>   `drive_run_progress_through_os_calls`. Snapshots stay mount-free. (§5.1)
> - Overlay state and `write_bytes_used` are **cumulative on the mount, not
>   per-run.** Discard = construct a fresh mount. (§4)
> - Single whole-table `Mutex<Option<MountTable>>` resource design. (§5.2)
> - `add/5` returns `{:ok, t} | {:error, reason}`; `add!/5` raises. (§2)
> - Unhandled FS calls use `OsFunction::on_no_handler` semantics. (§5.1)

## Problem

Today ExMonty offers two modes of filesystem access for sandboxed Python:

1. **`ExMonty.PseudoFS`** — pure in-memory virtual files supplied as Elixir
   data. Fully isolated; the sandbox cannot read or write the host disk.
2. **Custom `:os` handlers** — a `%{atom => fn args, kwargs -> result}` map
   the user writes by hand. Powerful but requires reimplementing path security
   per-app.

Both leave a real-world use case uncovered: **"give the sandbox access to a
specific host directory, with a controlled access policy, and don't let it
escape."**

Concrete examples:

- *"Run user-supplied data scripts that may read CSVs from `/var/lib/myapp/data`,
  but must not see anything else on disk."* (read-only mount)
- *"Let the sandbox scribble freely, but discard everything when the run ends."*
  (overlay mount + discard mount object after run)
- *"Sandbox can write outputs to `/var/lib/myapp/output`, capped at 10 MB
  total across runs against this mount object."* (read-write mount with
  `write_bytes_limit`)

Upstream monty has shipped a complete sandboxed-mount implementation (PR #305
in v0.0.13, fixes through v0.0.17). It enforces path canonicalisation,
boundary checks, and symlink-escape detection. We just haven't wired it up.

## 2. Proposed API

```elixir
{:ok, mounts} = ExMonty.Mount.new()
{:ok, mounts} = ExMonty.Mount.add(mounts, "/data",   "/var/lib/myapp/data",   :read_only)
{:ok, mounts} = ExMonty.Mount.add(mounts, "/scratch","/tmp/sandbox-scratch",  :overlay)
{:ok, mounts} = ExMonty.Mount.add(mounts, "/output", "/var/lib/myapp/output", :read_write,
                  write_bytes_limit: 10_000_000)

ExMonty.Sandbox.run(code, mounts: mounts, os: %{getenv: &my_getenv/2})
```

Or pipe-friendly with `add!/5` (raises on error):

```elixir
mounts =
  ExMonty.Mount.new!()
  |> ExMonty.Mount.add!("/data",    "/var/lib/myapp/data",    :read_only)
  |> ExMonty.Mount.add!("/scratch", "/tmp/sandbox-scratch",   :overlay)
  |> ExMonty.Mount.add!("/output",  "/var/lib/myapp/output",  :read_write,
       write_bytes_limit: 10_000_000)

ExMonty.Sandbox.run(code, mounts: mounts)
```

From the Python side:

```python
from pathlib import Path

# Reads /var/lib/myapp/data/users.csv from the host
data = Path("/data/users.csv").read_text()

# Writes go into the in-memory overlay; host is never touched
Path("/scratch/intermediate.json").write_text("...")

# Writes hit /var/lib/myapp/output/results.csv on the host
Path("/output/results.csv").write_text(processed)

# Path that doesn't match any mount → PermissionError
Path("/etc/passwd").read_text()
```

### Mount modes

| Atom         | Behaviour                                                           |
|--------------|---------------------------------------------------------------------|
| `:read_only` | Reads passthrough to host. Writes raise `PermissionError`.          |
| `:read_write`| Full passthrough. Writes hit the real disk. **Footgun — see §6.**   |
| `:overlay`   | Reads fall through to host. Writes captured in-memory; host is untouched. Deletions create tombstones that hide real files for subsequent reads. |

### Module surface

```elixir
defmodule ExMonty.Mount do
  @opaque t :: %__MODULE__{ref: reference()}
  @opaque lease :: %__MODULE__.Lease{ref: reference()}

  @type mode :: :read_only | :read_write | :overlay
  @type add_opts :: [write_bytes_limit: pos_integer()]
  @type add_error ::
          :invalid_virtual_path           # not absolute, contains "..", etc.
          | :invalid_mode                  # mode atom not in @type mode
          | :host_path_not_found
          | :host_path_not_directory
          | :host_path_canonicalize_failed
          | :mount_in_use                  # mount is currently leased to a run
          | {:already_mounted, virtual :: String.t()}

  @type list_entry :: %{
          virtual: String.t(),
          host: String.t(),
          mode: mode(),
          write_bytes_used: non_neg_integer(),
          write_bytes_limit: pos_integer() | :unlimited
        }

  # ── Construction ────────────────────────────────────────────────────────

  @spec new() :: {:ok, t()} | {:error, term()}
  @spec new!() :: t()

  @spec add(t(), virtual :: String.t(), host :: String.t(), mode(), add_opts()) ::
          {:ok, t()} | {:error, add_error()}
  @spec add!(t(), virtual :: String.t(), host :: String.t(), mode(), add_opts()) :: t()

  # Permissive — works whether or not the mount is leased.
  # Lightweight descriptors are stored on the source resource, separate
  # from the leased table.
  @spec count(t()) :: non_neg_integer()

  # `list/1` requires the live table (it surfaces `write_bytes_used`);
  # errors if the mount is currently leased. `list!/1` raises.
  @spec list(t()) :: {:ok, [list_entry()]} | {:error, :mount_in_use}
  @spec list!(t()) :: [list_entry()]

  # ── Run lease (advanced API; Sandbox.run handles this internally) ───────

  @spec checkout(t()) :: {:ok, lease()} | {:error, :mount_in_use}

  # Idempotent: calling `release/1` on an already-released lease returns
  # `:ok` and is a no-op.
  @spec release(lease()) :: :ok
end
```

`add/5` and `add!/5` mutate the underlying resource and return the same `t()`
(resource ref unchanged). The `{:ok, t}` wrapping makes failure modes
explicit while still letting users pipe with `add!/5`.

A `lease` is a separate opaque resource. While a lease is alive, the source
`%Mount{}` is "in use": `add/5`, `list/1`, `checkout/1` against it all return
`{:error, :mount_in_use}`. Releasing the lease (or letting it be GC'd) puts
the mount table back. **Most users never touch `checkout` / `release` —
`Sandbox.run` does it for you with proper `try/after` semantics.**

## 3. Composition with `:os`

`mounts:` is a **new, separate option** to `Sandbox.run`. It composes with
existing `:os` shapes:

| Option combination                              | Behaviour                                              |
|-------------------------------------------------|--------------------------------------------------------|
| `mounts: mounts`                                | mount-only; unmounted paths get `PermissionError`     |
| `mounts: mounts, os: %{...}` (function map)     | mounts first, fallback to function map for unmounted  |
| `mounts: mounts, os: pseudo_fs`                 | mounts first, fallback to PseudoFS for unmounted (see §3.1) |
| `mounts: mounts, handler: MyHandler`            | mounts first, fallback to `MyHandler.handle_os/3`     |
| `os: ...` (no mounts)                           | (unchanged) existing behaviour                         |

Order of resolution for an OS call:
1. If filesystem op AND path matches a mount → mount handles it.
2. If filesystem op AND no mount matches → fallback (`:os` map / PseudoFS /
   handler module).
3. If non-filesystem op (e.g. `:getenv`, `:date_today`) → fallback only.
4. No mount AND no fallback → `OsFunction::on_no_handler` —
   `PermissionError` for FS ops, `RuntimeError` for non-FS ops.

### 3.1 Mounts + PseudoFS

Allowed but flagged in docs as unusual. Mounts handle real-host paths;
PseudoFS handles purely virtual paths. If a path matches both a mount prefix
and a PseudoFS file, the mount wins (rule 1). This means PseudoFS effectively
covers paths that don't fall under any mount. Document with an example.

### 3.2 Why composition won the call

Reviewer point: mutual exclusion blocks common cases like
"mounts + custom `getenv`" or "mounts + `datetime_now` clock handler." Those
non-FS handlers don't conflict with mount semantics — they're orthogonal —
and forcing the user to write a handler module that delegates to mounts is
not actually possible since the mount-table internals are private. Separate
options + clear ordering is simpler than tagged-map composition (`os: %{mounts: ..., fallback: ...}`)
and easier to document.

## 4. Lifecycle semantics

A `Mount` is **stateful**. Two pieces of state persist on the mount object
across runs:

1. **Overlay storage** (`:overlay` mode only). Writes accumulate in-memory on
   the mount. A subsequent run against the same mount object sees those
   writes.
2. **`write_bytes_used` counter** (any mode with a `write_bytes_limit`). The
   counter is **cumulative across runs against this mount object** — it does
   not reset between runs. When `write_bytes_used + new_write_size >
   write_bytes_limit`, the write raises `OSError`.

> **To discard overlay state or reset the write counter, create a fresh mount.**
> There is no `reset/1` in v1. Mount construction is cheap.

### Per-run flow

The unit of mount tenancy is the **logical run**, not the NIF segment.

```elixir
# What Sandbox.run does internally:
case ExMonty.Mount.checkout(mounts) do
  {:ok, lease} ->
    try do
      # Loop:
      #   ExMonty.start_with_mounts(runner, inputs, limits, lease)
      #   ExMonty.resume_with_mounts(snapshot, result, lease)
      #   ...until {:complete, value, output}
      drive_loop(lease)
    after
      ExMonty.Mount.release(lease)
    end

  {:error, :mount_in_use} ->
    {:error, :mount_in_use}
end
```

Why a single lease for the whole run, not per-NIF re-borrow:

- A `start_with_mounts` call may pause for a `function_call` /
  `name_lookup` / `os_call` (non-FS or unmounted) progress event, returning
  control to Elixir.
- The Elixir handler may take arbitrary time. Without a run-lifetime lease,
  another `Sandbox.run` could grab the mount during that window.
- Worse, a second run could *write* to the overlay while the first is
  paused, breaking its consistency guarantees.

The lease holds the `MountTable` for the entire logical run. NIFs accept
`lease` as a parameter and operate on the lease's owned table without locking
or unwrapping the source resource.

### Lease consistency rules

While a lease is alive against a mount:

| Operation                        | Result                                  |
|----------------------------------|-----------------------------------------|
| `Mount.add/5` (same mount)       | `{:error, :mount_in_use}`               |
| `Mount.list/1` (same mount)      | `{:error, :mount_in_use}`               |
| `Mount.list!/1` (same mount)     | raises `ExMonty.MountInUseError`        |
| `Mount.count/1` (same mount)     | proceeds (reads side-channel descriptors, §5.2) |
| `Mount.checkout/1` (same mount)  | `{:error, :mount_in_use}` (second run)  |
| `start_with_mounts(..., lease)`  | proceeds                                |
| `resume_with_mounts(..., lease)` | proceeds                                |

Once the lease is released, the mount returns to its pre-run shape: any
overlay writes and `write_bytes_used` increments from the run are now
visible to subsequent calls.

### Backup safety: lease Drop guard

If a process abandons a lease without calling `release/1`, the Rust
`Drop` impl on the lease resource puts the table back into the source
mount when the lease becomes unreachable to the BEAM GC. **Drop is a
backup, not a substitute for `try/after`:**

- Drop runs only when the lease resource is no longer referenced by any
  process. There is no guarantee about *when* that happens — it depends
  on GC pressure.
- `Sandbox.run` therefore calls `release/1` explicitly in an `after`
  block. That makes the mount available for the *next sequential run*
  in the same process without waiting for GC.
- If the BEAM process holding the lease is killed before
  `Sandbox.run`'s `after` runs, the lease eventually drops and the
  mount becomes usable again — but a back-to-back retry in another
  process may race with that drop.

`release/1` is **idempotent** (§5.2), so explicit release followed by
Drop on the same lease is safe.

## 5. Implementation outline

### 5.1 Mount-aware loop in Rust

Today's flow (without mounts) — `Sandbox.run/2` calls `ExMonty.start/3` then
loops on `ExMonty.resume/2`. Snapshots carry no mount context.

New flow with mounts — Sandbox.run holds a lease for the duration; NIFs
accept the lease ref. Mount-handled FS ops are resumed inside Rust without
returning to Elixir. Non-mount progress (function calls, name lookups,
`os_call` for non-FS or unmounted paths) returns to Elixir for dispatch.

```
Elixir                     Rust                            VM
─────────────────────────  ──────────────────────────────  ───────────
  Mount.checkout(m)        check & swap MountTable
  ──────────────────────>  out of source resource into
                           lease resource
  <─────────────────────── lease

  start_with_mounts(lease) borrow lease.table              start
  ──────────────────────>                                  ↓
                           loop:                           os_call?
                             ┌─ mount handles ──> resume   │
                             │                       ↓     │
                             │                    progress │
                             │                       ↓     │
                             │  ┌── (loop back) ───────────┘
                             │  ↓
                             └─ unmounted / non-FS ──────> break
                           release borrow
  <─────────────────────── progress (or unmounted OsCall)

  (Elixir dispatches non-mount call, computes result or :no_handler)

  resume_with_mounts(lease, result_or_:no_handler)
  ──────────────────────>  borrow lease.table              resume
                           if result == :no_handler:       (loop back as above)
                             obj = call.function
                                   .on_no_handler(args)
                             call.resume(Error(obj), ...)
                           ...
  ...

  Mount.release(lease)     swap MountTable back into source resource
  ──────────────────────>
```

Two new NIFs:

```rust
#[rustler::nif(schedule = "DirtyCpu")]
fn start_with_mounts<'a>(
    env: Env<'a>,
    runner: ResourceArc<RunnerResource>,
    inputs: Vec<(String, Term<'a>)>,
    limits: Term<'a>,
    lease: ResourceArc<MountLease>,
) -> NifResult<Term<'a>>;

#[rustler::nif(schedule = "DirtyCpu")]
fn resume_with_mounts<'a>(
    env: Env<'a>,
    snapshot: ResourceArc<SnapshotResource>,
    result: Term<'a>,                  // {:ok, value} | {:error, ...} | :no_handler
    lease: ResourceArc<MountLease>,
) -> NifResult<Term<'a>>;
```

Plus the lease-management NIFs:

```rust
#[rustler::nif] fn mounts_checkout(m: ResourceArc<MountResource>)
    -> NifResult<ResourceArc<MountLease>>;
#[rustler::nif] fn mounts_release(lease: ResourceArc<MountLease>)
    -> NifResult<rustler::Atom>;
```

Each loop iteration mirrors upstream's `drive_run_progress_through_os_calls`
(`crates/monty-python/src/monty_cls.rs:2077`):

- Borrow the lease's `MountTable` for the duration of this NIF call.
- While `RunProgress::OsCall(call)` arrives:
  - Ask `MountTable::handle_os_call`.
  - `Some(Ok(obj))` → `call.resume(Return(obj), print)`, continue.
  - `Some(Err(mount_err))` → `call.resume(Error(mount_err.into_exception()), print)`,
    continue.
  - `None` (non-FS, or FS path that didn't match any mount) → break out,
    return the OsCall to Elixir for fallback dispatch.
- Any other progress variant → break out, return to Elixir.
- All normal success/error paths drop the lease borrow cleanly. A `Drop`
  guard on the borrow handle ensures the lease's table is intact for the
  next NIF call even if a Rust panic unwinds the loop body.

### `:no_handler` from Elixir → `OsFunction::on_no_handler` in Rust

When the Elixir-side `Sandbox.dispatch_os` finds no matching `:os` handler
for an unmounted or non-FS op, it returns `:no_handler` as the result term
instead of synthesising an exception. The Rust `resume_with_mounts` NIF
sees this and calls `call.function.on_no_handler(&call.args)` to build the
canonical exception (`PermissionError` for FS, `RuntimeError` for non-FS),
then resumes the snapshot with that exception.

Reasons:
- Avoids duplicating upstream exception text and types in Elixir.
- Stays in sync automatically when upstream tweaks `on_no_handler`.
- Makes the "no handler available" case observable as a single distinct
  signal, not a special exception synthesised by ExMonty.

### 5.2 Resource design

Two Rustler resources: `MountResource` (the durable mount object backing a
`%Mount{}`) and `MountLease` (the per-run lease backing a `%Mount.Lease{}`).

#### `MountResource`

```rust
pub struct MountResource {
    // The live table. `None` while a lease is active.
    // Access is gated by `take_lock` to serialise checkout/release.
    table: Mutex<Option<MountTable>>,

    // Lightweight, always-available metadata. Mirrors the mounts in the
    // table but doesn't depend on the table being present. `count/1` and
    // any non-mutable inspection reads from here without contending with
    // an active run. `add/5` updates this in lockstep with the table.
    descriptors: Mutex<Vec<MountDescriptor>>,
}

struct MountDescriptor {
    virtual_path: String,
    host_path: String,
    mode_label: &'static str,    // "read-only" | "read-write" | "overlay"
    write_bytes_limit: Option<u64>,
}
```

`count/1` locks `descriptors` only — never the table — so it works
during a lease. `list/1` needs `write_bytes_used` (which lives on the
`Mount` inside the table), so it locks the table and errors with
`:mount_in_use` when the table has been moved into a lease.

#### `MountLease`

```rust
pub struct MountLease {
    // Source resource, kept alive for the lease's duration so Drop can
    // restore the table. Holding a `ResourceArc` prevents the source
    // `MountResource` from being GC'd while a lease is outstanding.
    source: ResourceArc<MountResource>,

    // The owned table for this lease. `None` after release.
    table: Mutex<Option<MountTable>>,

    // CAS flag so concurrent `release/1` calls (or release + Drop)
    // serialise to a single put-back. See `release_inner` below.
    released: AtomicBool,
}
```

The `Mutex` on `table` plus the `AtomicBool` on `released` mean a lease
tolerates concurrent NIF calls or `release/1` calls from different
processes. We can't assume the opaque `lease()` term stays in one
process — opaque resources can be sent in messages — so the Rust side
is built to handle that.

#### Idempotent release path

```rust
fn release_lease_inner(lease: &MountLease) {
    // Single CAS winner moves the table back. Subsequent callers see
    // `released == true` and short-circuit.
    if lease.released.swap(true, Ordering::AcqRel) {
        return;  // Already released — no-op.
    }

    // Take the table out of the lease slot under its mutex.
    let table = match lease.table.lock() {
        Ok(mut guard) => guard.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    };

    if let Some(table) = table {
        // Restore into the source resource.
        if let Ok(mut source_slot) = lease.source.table.lock() {
            *source_slot = Some(table);
        }
        // If the source mutex is poisoned we deliberately drop the
        // table here — better than panicking inside Drop. The mount
        // resource is then "permanently leased" until GC'd; surfaced
        // as `:mount_in_use` for any subsequent op.
    }
}

#[rustler::nif]
fn mounts_release(lease: ResourceArc<MountLease>) -> rustler::Atom {
    release_lease_inner(&lease);
    atoms::ok()
}

impl Drop for MountLease {
    fn drop(&mut self) {
        // Same path as explicit release; the CAS handles double-release
        // and "release then Drop" cases identically.
        release_lease_inner(self);
    }
}
```

This shape closes three concrete failure modes:
- `release/1` called twice → second call sees `released == true` and
  returns `:ok` immediately.
- `release/1` followed by `Drop` (the `try/after` + GC path) → Drop is
  a no-op because the CAS already flipped.
- Two processes holding the lease term and both calling `release/1`
  concurrently → only one wins the CAS and moves the table; the other
  returns `:ok` without touching state.

#### Why whole-table, not per-mount slots

(Unchanged from revision 2/3.)

- Upstream sorts mounts longest-prefix-first inside the table; per-slot
  put-back has to preserve order or rebuild the sort. Whole-table
  take/put-back avoids the question.
- Concurrent `add/5` versus a running sandbox needs synchronisation
  either way. One mutex per resource is simpler than reasoning about
  per-slot lock ordering.
- Upstream's `take_shared_mounts` API exists because monty-python's
  `MountDir` objects can be passed across multiple concurrent
  `Monty.run` calls. We don't have that fan-out — one `Mount` resource
  is one mount table.

### 5.3 Elixir surface changes

- `lib/ex_monty/mount.ex` — new module, ~150 lines including specs and
  moduledoc. Plus `ExMonty.Mount.Lease` opaque struct and
  `ExMonty.MountInUseError` exception.
- `lib/ex_monty/native.ex` — declarations for new NIFs:
  - `mounts_new/0`
  - `mounts_add/5`
  - `mounts_list/1`
  - `mounts_count/1`
  - `mounts_checkout/1`
  - `mounts_release/1`
  - `start_with_mounts/4`
  - `resume_with_mounts/3`
- `lib/ex_monty/sandbox.ex`:
  - `run/2` accepts new `:mounts` option (default `nil`).
  - When `:mounts` present:
    - Calls `Mount.checkout/1`. On `{:error, :mount_in_use}`, returns the
      error directly to the caller (no `try/after` wraps a no-op).
    - On `{:ok, lease}`, wraps the inner loop in `try/after` with
      `Mount.release(lease)` in the `after` block.
    - Inner loop calls `start_with_mounts` / `resume_with_mounts` (passing
      the lease).
    - `dispatch_os/4` returns `:no_handler` when nothing matches an
      unmounted/non-FS call, instead of `{:error, :os_error, msg}`. Rust
      handles the conversion to `OsFunction::on_no_handler` semantics.
  - When `:mounts` absent, behaviour is unchanged.

### 5.4 Tests

`test/ex_monty/mount_test.exs`:

**Construction & validation:**
- `new/0` returns `{:ok, t}`; `new!/0` returns `t`.
- `add/5` rejects invalid virtual paths (relative, `".."`, empty).
- `add/5` rejects nonexistent host paths.
- `add/5` rejects host paths that aren't directories.
- `add/5` returns `{:error, :mount_in_use}` if the resource is currently
  in use by a run.
- `add!/5` raises on each error case above.

**Mode behaviour:**
- `:read_only` — reads succeed, writes raise `PermissionError`.
- `:read_write` — reads and writes both hit the host.
- `:overlay` — reads fall through, writes captured, host untouched, tombstones
  hide deleted host files.

**Routing:**
- Longest-prefix-first wins (`/data/users` mount preferred over `/data`).
- Cross-mount `Path.rename` requires both endpoints to resolve to the same
  mount.
- Unmounted path raises `PermissionError` (not generic `os_error`).

**Security:**
- Symlink escape blocked (`/data/link → /etc/passwd`).
- `..` segments cannot escape the mount root.
- Absolute symlinks pointing outside the host root are blocked.

**Lifecycle & limits:**
- Cumulative `write_bytes_used` across multiple runs against the same mount
  object.
- `write_bytes_limit` enforced cumulatively, not per run.
- Overlay writes from run A are visible in run B against the same mount.
- Fresh mount object discards overlay state from a previous mount.
- Mount object is reusable after a Python exception in a previous run.

**Lease semantics:**
- `Mount.checkout/1` succeeds; second concurrent `checkout/1` returns
  `{:error, :mount_in_use}`.
- `Mount.add/5` against a leased mount returns `{:error, :mount_in_use}`.
- `Mount.list/1` against a leased mount returns `{:error, :mount_in_use}`.
- `Mount.list!/1` against a leased mount raises `ExMonty.MountInUseError`.
- `Mount.count/1` against a leased mount **succeeds** (descriptor
  side-channel) and returns the same count as before the lease.
- After `Mount.release/1`, the mount is fully usable again — `add`, `list`,
  another `checkout` all succeed.
- After a `Sandbox.run` succeeds, the mount is released even though the
  caller never sees the lease.
- After a `Sandbox.run` fails (e.g. Python exception, NIF error), the mount
  is still released (try/after).
- An interleave attempt during a paused callback errors cleanly: in run A,
  `start_with_mounts` returns a `function_call` progress; before the handler
  resumes, run B's `Mount.checkout/1` returns `{:error, :mount_in_use}`.
- **Idempotency:** `Mount.release/1` on an already-released lease returns
  `:ok` and is a no-op; subsequent `add/5` against the source mount
  succeeds.
- **Concurrent release:** two processes holding the same lease term
  calling `release/1` simultaneously both observe `:ok`, only one
  effects the put-back, and the source mount ends up in a usable state.
- **Drop GC fallback:** drop all references to a lease without calling
  `release/1`; after a forced GC, the mount is usable again. (Use
  `:erlang.garbage_collect/0` to make the test deterministic.)
- **Process death:** spawn a linked process that takes a lease and
  exits without releasing; verify the mount becomes usable once the
  lease term is unreachable.

**Composition with `:os`:**
- Mounts + `getenv` function-map handler: `getenv` works, mount paths handled.
- Mounts + `datetime_now` handler: `datetime_now` works, mount paths handled.
- Mounts + handler module: `handle_os/3` invoked for unmounted paths only.
- Mounts + PseudoFS: mount paths win over PseudoFS for overlapping prefixes.

Plus updates to `test/ex_monty/sandbox_test.exs` for the new option and its
interaction with existing options.

## 6. Read-write mode safety

`:read_write` lets sandbox code modify real host files. That's a real footgun.

**Decision: documented, unguarded.** Same posture as `File.write/2` in core
Elixir. Every example in the docs uses `:read_only` or `:overlay`;
`:read_write` only appears with a "use with caution" callout.

Considered but rejected: renaming to `:dangerous_read_write`. ExMonty is
positioned for executing **untrusted Python** — a user wiring up
`:read_write` has already accepted that posture. Renaming would be theatre,
not safety.

## 7. Out of scope for v1

Revisit only with concrete user demand:

1. **Persistent overlay state across BEAM restarts.** Today's overlay is
   in-memory only. Saving and restoring overlay state (postcard? sqlite?)
   is tractable but separate.
2. **Per-mount resource limits beyond `write_bytes_limit`.** Read-bytes
   limit, inode count, etc. — upstream doesn't expose them today.
3. **Mount reset.** No `Mount.reset/1` to clear overlay state in place.
   Construct a fresh mount instead.
4. **Network-style mounts** (HTTP-backed FS, S3, etc.) — not in upstream
   monty's scope.
5. **Tagged-map composition** (`os: %{mounts: ..., fallback: ...}`) — the
   separate `mounts:` option in §3 covers the same use case more cleanly.
6. **`overlay_bytes` field on `list/1` entries.** Upstream `OverlayState`
   in v0.0.17 doesn't expose a public byte count. Computing it locally
   would require either an upstream API addition or an approximation
   complicated by tombstones, directories, and lazy `RealFileRef` entries.
   `write_bytes_used` covers the per-mount quota observability use case.
7. **Inline mount-spec shorthand** (`mounts: [{"/data", "/host", :read_only}, ...]`).
   Hides the key semantic that mounts are stateful resources — every
   `Sandbox.run` call would silently allocate fresh mounts and discard
   overlay state. If we want convenience later, prefer
   `ExMonty.Mount.from_specs/1` returning a real `%Mount{}` rather than
   inlining at the `Sandbox.run` boundary.

## 8. Open questions

All resolved through the review cycle. Recorded for the implementation
plan:

- **`count/1` is permissive** (works under lease via the descriptor
  side-channel in §5.2). Resolved revision 4.
- **`list/1` errors under lease** (it surfaces live `write_bytes_used`).
  Resolved revision 3.
- **`overlay_bytes` field on `list/1` entries**: deferred (§7).
- **Inline mount-spec shorthand**: deferred (§7).
- **`mounts:` as separate Sandbox option** (vs. mutually-exclusive `:os`):
  resolved revision 2 — separate option.
- **`:no_handler` round-trip Elixir → Rust**: resolved revision 3.
- **Idempotent `release/1` and concurrent-tolerant lease**: resolved
  revision 4.

## 9. Effort estimate

Roughly 3–4 days for a careful implementation:

- 0.75 day Rust resource design (`MountResource` with descriptor
  side-channel, `MountLease` with `AtomicBool` + `Mutex`) and the
  `mounts_*` NIFs including idempotent, concurrent-tolerant
  `checkout` / `release`.
- 1 day mount-aware `start_with_mounts` / `resume_with_mounts` loop
  including the lease-borrow guard, `:no_handler` round-trip, and the
  upstream `drive_run_progress_through_os_calls` integration.
- 0.5 day Elixir `Mount` module + `Sandbox` integration (`try/after`
  around the lease, `dispatch_os` returning `:no_handler`).
- 1 day tests:
  - 0.5 day security tests (path escape, symlink escape, cross-mount
    rename, invalid path).
  - 0.5 day lease semantics tests (concurrent checkout, list/add under
    lease, post-error reuse, idempotent release, double-release,
    concurrent-process release, GC-driven Drop fallback).
- 0.5 day docs, examples, CHANGELOG, README.

## 10. References

- Upstream `MountTable`: `crates/monty/src/fs/mount_table.rs`
- Upstream mount-aware loop:
  `crates/monty-python/src/monty_cls.rs:2077` (`drive_run_progress_through_os_calls`)
- Upstream `OsFunction::on_no_handler`: `crates/monty/src/os.rs`
- Upstream tests: `crates/monty-python/tests/test_mount_table.py`
- This proposal's revision 1: see git history for `proposals/MOUNT_TABLE.md`
