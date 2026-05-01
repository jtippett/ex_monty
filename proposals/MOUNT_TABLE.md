# Proposal: `ExMonty.Mount` — host filesystem mounts in the sandbox

**Status:** Draft, revision 3 (after second review pass)
**Author:** James Tippett
**Target:** ExMonty v0.4 (post-v0.0.17 update)

> **Revision 3 changes (incorporating second review):**
> - **Explicit run lease.** `Mount.checkout/1` takes the table out for the
>   *entire logical run*; NIFs operate on the lease, never the bare mount.
>   Closes the gap where two `Sandbox.run`s could interleave on the same
>   mount while one was paused in an Elixir callback. (§2, §4)
> - **`:no_handler` is computed Rust-side.** Elixir dispatch returns a
>   `:no_handler` atom; the Rust loop calls `OsFunction::on_no_handler`
>   itself. No duplication of upstream exception text in Elixir. (§5.1)
> - **`:mount_in_use` added to `add_error` type and to `list/1`'s contract.**
> - **Toned-down panic claims.** "Drop guard for unwind safety" rather than
>   promising panic recovery. (§5.1)
> - **Open questions resolved.** `overlay_bytes` deferred (upstream
>   `OverlayState` doesn't expose a public byte count; tombstones, dirs, lazy
>   refs make accounting non-trivial). Inline mount-spec shorthand deferred
>   (hides the stateful-resource nature; if we want it, do it as
>   `Mount.from_specs/1` not `Sandbox.run(mounts: [...])`). (§7, §8)
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

  @spec count(t()) :: non_neg_integer()

  # `list/1` errors if the mount is currently leased to a run; `list!/1` raises.
  @spec list(t()) :: {:ok, [list_entry()]} | {:error, :mount_in_use}
  @spec list!(t()) :: [list_entry()]

  # ── Run lease (advanced API; Sandbox.run handles this internally) ───────

  @spec checkout(t()) :: {:ok, lease()} | {:error, :mount_in_use}
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
| `Mount.count/1` (same mount)     | also `{:error, :mount_in_use}`? — see §8.1 |
| `Mount.checkout/1` (same mount)  | `{:error, :mount_in_use}` (second run)  |
| `start_with_mounts(..., lease)`  | proceeds                                |
| `resume_with_mounts(..., lease)` | proceeds                                |

Once the lease is released, the mount returns to its pre-run shape: any
overlay writes and `write_bytes_used` increments from the run are now
visible to subsequent calls.

### Backup safety: lease Drop guard

If the BEAM process holding a lease dies before calling `release/1`, the
lease's Rust `Drop` impl puts the table back into the source mount. The
lease resource is owned by exactly one BEAM process at a time (it's not
shared across `ResourceArc`s in the user-facing API), so Drop runs
deterministically when the lease becomes unreachable.

This is a backup, not a substitute for `try/after` — `release/1` ensures
the mount becomes available for the next run *before* the NIF stack
unwinds, which matters for sequential runs in the same process.

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

```rust
pub struct MountResource {
    // The lease takes the inner table out via `Option::take`. While
    // `table` is `None`, the mount is leased and all read/write ops
    // against it return `:mount_in_use`.
    table: Mutex<Option<MountTable>>,
}

pub struct MountLease {
    // Owning reference back to the source resource, used by `Drop` to
    // restore the table even if `release/1` was never called.
    source: ResourceArc<MountResource>,
    // Owned table for the lease's lifetime.
    table: Mutex<Option<MountTable>>,
}

impl Drop for MountLease {
    fn drop(&mut self) {
        // Backup safety net: if release/1 wasn't called (e.g. the BEAM
        // process holding the lease died), put the table back into the
        // source resource so it can be used again.
        if let Ok(mut lease_slot) = self.table.lock() {
            if let Some(table) = lease_slot.take() {
                if let Ok(mut source_slot) = self.source.table.lock() {
                    *source_slot = Some(table);
                }
            }
        }
    }
}
```

Reasons for the whole-table design (rather than per-mount
`Arc<Mutex<Option<Mount>>>` slots):

- Upstream sorts mounts longest-prefix-first inside the table; per-slot
  put-back has to preserve order or rebuild the sort. Whole-table
  take/put-back avoids the question.
- Concurrent `add/5` versus a running sandbox needs synchronisation either
  way. One mutex per resource is simpler than reasoning about per-slot
  lock ordering.
- Upstream's `take_shared_mounts` API exists because monty-python's
  `MountDir` objects can be passed across multiple concurrent `Monty.run`
  calls. We don't have that fan-out — one `Mount` resource is one mount
  table — so the simpler whole-table model fits.

The `MountLease` keeping a `ResourceArc<MountResource>` ensures that as
long as a lease is alive, the source `Mount` resource cannot be GC'd
out from under it.

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
- After `Mount.release/1`, the mount is fully usable again — `add`, `list`,
  another `checkout` all succeed.
- After a `Sandbox.run` succeeds, the mount is released even though the
  caller never sees the lease.
- After a `Sandbox.run` fails (e.g. Python exception, NIF error), the mount
  is still released (try/after).
- An interleave attempt during a paused callback errors cleanly: in run A,
  `start_with_mounts` returns a `function_call` progress; before the handler
  resumes, run B's `Mount.checkout/1` returns `{:error, :mount_in_use}`.
- Lease Drop fallback: if the BEAM process holding a lease exits abruptly
  (process kill mid-run), the lease's Drop puts the table back. (Test via
  spawned linked process + `Process.exit/2`.)

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

## 8. Open questions for reviewers

Both revision-2 questions resolved (`overlay_bytes` and inline shorthand
both deferred to §7). One small one remaining:

### 8.1 Should `Mount.count/1` error under lease?

`count/1` returns the number of mounts. It doesn't read mutable state of
the table itself — number of mounts is fixed at the point the lease was
taken (since `add/5` errors during a lease).

Two options:

- **Strict**: `count/1` returns `{:error, :mount_in_use}` like `list/1`,
  for consistency. Forces all mount inspection through the same gate.
- **Permissive**: `count/1` returns the count, since it's stable for the
  lease's duration. Makes "how many mounts did I configure" trivially
  observable from monitoring code without needing to coordinate with runs.

I'd say **permissive**, exposed as a fast read that doesn't lock the
underlying table — but flagging because §4 currently lists it as
`:mount_in_use`-on-lease for consistency. Push either way.

## 9. Effort estimate

Roughly 2.5–3.5 days for a careful implementation:

- 0.5 day Rust resource design (`MountResource` + `MountLease`) and
  `mounts_*` NIFs including `checkout` / `release` with Drop-guard backup.
- 1 day mount-aware `start_with_mounts` / `resume_with_mounts` loop
  including the lease-borrow guard, `:no_handler` round-trip, and the
  upstream `drive_run_progress_through_os_calls` integration.
- 0.5 day Elixir `Mount` module + `Sandbox` integration (`try/after`
  around the lease, `dispatch_os` returning `:no_handler`).
- 1 day tests:
  - 0.5 day security tests (path escape, symlink escape, cross-mount
    rename, invalid path).
  - 0.5 day lease semantics tests (concurrent checkout, list/add under
    lease, post-error reuse, Drop fallback via process kill).
- 0.5 day docs, examples, CHANGELOG, README.

## 10. References

- Upstream `MountTable`: `crates/monty/src/fs/mount_table.rs`
- Upstream mount-aware loop:
  `crates/monty-python/src/monty_cls.rs:2077` (`drive_run_progress_through_os_calls`)
- Upstream `OsFunction::on_no_handler`: `crates/monty/src/os.rs`
- Upstream tests: `crates/monty-python/tests/test_mount_table.py`
- This proposal's revision 1: see git history for `proposals/MOUNT_TABLE.md`
