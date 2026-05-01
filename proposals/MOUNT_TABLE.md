# Proposal: `ExMonty.Mount` — host filesystem mounts in the sandbox

**Status:** Draft, revision 2 (after first review pass)
**Author:** James Tippett
**Target:** ExMonty v0.4 (post-v0.0.17 update)

> **Revision 2 changes (incorporating reviewer feedback):**
> - Mounts are now a **separate `mounts:` option**, composing with `:os`, not
>   mutually exclusive with it. (§3)
> - **Mount-aware loop happens in Rust** via a new NIF mirroring upstream's
>   `drive_run_progress_through_os_calls`. Existing `start`/`resume` snapshots
>   stay mount-free. (§5.1)
> - Overlay state and `write_bytes_used` are **cumulative on the mount, not
>   per-run.** Users discard by creating a fresh mount. (§4)
> - Resource design switched from per-mount `Arc<Mutex<Option<Mount>>>` slots
>   to a whole-table `Mutex<Option<MountTable>>`. (§5.2)
> - `add/5` returns `{:ok, t} | {:error, reason}`; `add!/5` raises for
>   pipe-friendly use. (§2)
> - Unhandled FS calls fall through to `OsFunction::on_no_handler`
>   (`PermissionError` on FS, `RuntimeError` on non-FS) instead of ExMonty's
>   generic `:os_error`. (§5.1)
> - Test list expanded to cover unmounted-path denial, fallback composition,
>   overlay persistence, cumulative limits, add-while-in-use, post-error
>   reuse, invalid paths, and cross-mount rename. (§5.4)

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

  @type mode :: :read_only | :read_write | :overlay
  @type add_opts :: [write_bytes_limit: pos_integer()]
  @type add_error ::
          :invalid_virtual_path           # not absolute, contains "..", etc.
          | :host_path_not_found
          | :host_path_not_directory
          | :host_path_canonicalize_failed
          | {:already_mounted, virtual :: String.t()}

  @spec new() :: {:ok, t()} | {:error, term()}
  @spec new!() :: t()

  @spec add(t(), virtual :: String.t(), host :: String.t(), mode(), add_opts()) ::
          {:ok, t()} | {:error, add_error()}
  @spec add!(t(), virtual :: String.t(), host :: String.t(), mode(), add_opts()) :: t()

  @spec count(t()) :: non_neg_integer()
  @spec list(t()) :: [%{
          virtual: String.t(),
          host: String.t(),
          mode: mode(),
          write_bytes_used: non_neg_integer(),
          write_bytes_limit: pos_integer() | :unlimited
        }]
end
```

`add/5` and `add!/5` both mutate the underlying resource and return the same
`t()` (resource ref unchanged). The `{:ok, t}` wrapping makes failure modes
explicit while still letting users pipe with `add!/5`.

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

### Per-run flow (Rust side)

The new `Sandbox.run_with_mounts` path:

1. NIF `mounts_run` takes ownership of the `MountTable` (via
   `Mutex::lock` + `Option::take` on the resource).
2. Drives the os-call loop in Rust (see §5.1). Mount-handled FS ops never
   leave Rust.
3. On every exit path (success, mount error, propagated MontyException,
   panic recovery, NIF unwinding), the `MountTable` is **put back** into the
   resource slot.
4. After the NIF returns, the `%ExMonty.Mount{}` struct is safe to reuse for
   another run.

### Concurrent use surfaces a clear error

If two runs try to use the same mount concurrently, the second gets:

```elixir
{:error, %ExMonty.Exception{
  type: :runtime_error,
  message: "mount table is already in use by another run"
}}
```

Users who want concurrent reads against the same host directory should create
two separate `Mount` objects pointing at the same host path.

### Adding mounts while a run is in progress

`Mount.add/5` against a mount currently in use by a run returns
`{:error, :mount_in_use}`. Users should construct mounts before kicking off
runs.

## 5. Implementation outline

### 5.1 Mount-aware loop in Rust

Today's flow (without mounts):

```
Elixir          Rust           VM
  start  ──>  start NIF  ──>  start
                              ↓
                              os_call?
                              ↓
                <── OsCall ── progress
  resume  ──>  resume NIF ──> resume
  ...           (loop back)
```

Today the `Sandbox.run/2` loop in `lib/ex_monty/sandbox.ex` (around line 172)
calls `ExMonty.start/3` then loops on `ExMonty.resume/2`. **Snapshots
currently carry no mount context.** We do *not* want to thread a mount-table
reference through every snapshot.

New flow with mounts:

```
Elixir              Rust                     VM
  start_with_mounts ──> start_with_mounts NIF ──> start
                          ↓
                          take MountTable
                          ↓
                          loop:
                            os_call?
                            ↓ yes
                            mount handles?  ──> resume in Rust ──> loop
                            ↓ no                   ↑
                            non-FS or unmounted ───┘
                          ↓
                          put back MountTable
                          ↓
              <── progress (FunctionCall / NameLookup / Complete / unmounted OsCall)
```

Two new NIFs:

```rust
#[rustler::nif(schedule = "DirtyCpu")]
fn start_with_mounts<'a>(
    env: Env<'a>,
    runner: ResourceArc<RunnerResource>,
    inputs: Vec<(String, Term<'a>)>,
    limits: Term<'a>,
    mounts: ResourceArc<MountResource>,
) -> NifResult<Term<'a>>;

#[rustler::nif(schedule = "DirtyCpu")]
fn resume_with_mounts<'a>(
    env: Env<'a>,
    snapshot: ResourceArc<SnapshotResource>,
    result: Term<'a>,
    mounts: ResourceArc<MountResource>,
) -> NifResult<Term<'a>>;
```

Internally these mirror upstream's `drive_run_progress_through_os_calls` in
`crates/monty-python/src/monty_cls.rs:2077`:

- Take the `MountTable` once on entry.
- Loop while `RunProgress::OsCall(call)` arrives:
  - Ask `MountTable::handle_os_call`.
  - `Some(Ok(obj))` → call `call.resume(Return(obj), print)` in-Rust, continue.
  - `Some(Err(mount_err))` → `call.resume(Error(mount_err.into_exception()), print)`,
    continue.
  - `None` (non-FS or unmounted path) → break out, return the OsCall to
    Elixir for fallback dispatch.
- On any other progress variant (`FunctionCall`, `NameLookup`,
  `ResolveFutures`, `Complete`) → break out, return to Elixir.
- **Always** put the `MountTable` back, including on Rust panics. Wrap the
  loop body in a guard struct whose `Drop` impl restores the slot.

Snapshots returned to Elixir for non-mount progress carry no mount
reference. The Elixir-side `Sandbox.run` loop calls `resume_with_mounts/3`
(passing `mounts` again) to re-enter the Rust loop. The mount table is
re-borrowed each time.

Unhandled os calls (no mount + no fallback) emit `OsFunction::on_no_handler`
as the result, which gives `PermissionError` for FS ops and `RuntimeError`
for non-FS ops. Replaces the current generic `:os_error`.

### 5.2 Resource design

Single whole-table `Mutex<Option<MountTable>>` rather than per-mount slots:

```rust
pub struct MountResource {
    table: Mutex<Option<MountTable>>,
}
```

Reasons:
- Upstream sorts mounts longest-prefix-first inside the table; per-slot
  put-back has to preserve order or rebuild the sort. Whole-table
  take/put-back avoids the question.
- Concurrent `add/5` versus a running sandbox needs synchronisation either
  way. One mutex is simpler than reasoning about per-slot lock ordering.
- Upstream's `take_shared_mounts` API exists for the case where mounts
  outlive a single `MontyRun` and might be used in multiple concurrent
  monty-python objects. We don't need that fan-out — one `Mount` resource
  is one mount table.

### 5.3 Elixir surface changes

- `lib/ex_monty/mount.ex` — new module, ~120 lines including specs and
  moduledoc.
- `lib/ex_monty/native.ex` — declarations for new NIFs:
  - `mounts_new/0`
  - `mounts_add/5`
  - `mounts_list/1`
  - `mounts_count/1`
  - `start_with_mounts/4`
  - `resume_with_mounts/3`
- `lib/ex_monty/sandbox.ex`:
  - `run/2` accepts new `:mounts` option (default `nil`).
  - When `:mounts` present, dispatch loop calls `start_with_mounts` /
    `resume_with_mounts` and uses `OsFunction::on_no_handler` semantics for
    unhandled.
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
- `:mount_in_use` error if a second run tries to use a mount mid-run.

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

## 8. Open questions for reviewers

Most of revision 1's open questions resolved by feedback. Two remain:

1. **`mounts_list/1` field shape.** I now include `write_bytes_used` and
   `write_bytes_limit` (per reviewer note about exposing cumulative state).
   Should it also include a per-mount `overlay_bytes` for `:overlay` mounts?
   Upstream tracks this; it'd help users monitor "how much memory am I
   accumulating." Cheap to add now.

2. **Should `Sandbox.run` accept a `mounts:` shorthand for an inline list?**
   I.e. `mounts: [{"/data", "/host/path", :read_only}, ...]` constructed
   on-the-fly. Convenient for one-shot runs, but loses cumulative overlay
   state semantics (every run gets a fresh mount). Could be added later as
   a `:mounts_inline` option without changing today's design. Defer?

## 9. Effort estimate

Roughly 2–3 days for a careful implementation:

- 0.5 day Rust resource design and `mounts_*` NIFs.
- 1 day mount-aware `start_with_mounts` / `resume_with_mounts` loop including
  the put-back guard and panic safety.
- 0.5 day Elixir `Mount` module + `Sandbox` integration.
- 1 day tests, especially the security tests (path escape, symlink escape).
- 0.5 day docs, examples, CHANGELOG, README.

## 10. References

- Upstream `MountTable`: `crates/monty/src/fs/mount_table.rs`
- Upstream mount-aware loop:
  `crates/monty-python/src/monty_cls.rs:2077` (`drive_run_progress_through_os_calls`)
- Upstream `OsFunction::on_no_handler`: `crates/monty/src/os.rs`
- Upstream tests: `crates/monty-python/tests/test_mount_table.py`
- This proposal's revision 1: see git history for `proposals/MOUNT_TABLE.md`
