# Proposal: `ExMonty.Mount` — host filesystem mounts in the sandbox

**Status:** Draft, seeking review
**Author:** James Tippett
**Target:** ExMonty v0.4 (post-v0.0.17 update)

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
  (overlay mount)
- *"Sandbox can write outputs to `/var/lib/myapp/output`, but is rate-limited to
  10 MB per run."* (read-write mount with write-bytes limit)

Upstream monty has shipped a complete sandboxed-mount implementation (PR #305
in v0.0.13, fixes through v0.0.17). It enforces path canonicalisation,
boundary checks, and symlink-escape detection. We just haven't wired it up.

## Proposed API

```elixir
mounts =
  ExMonty.Mount.new()
  |> ExMonty.Mount.add("/data", "/var/lib/myapp/data", :read_only)
  |> ExMonty.Mount.add("/scratch", "/tmp/sandbox-scratch", :overlay)
  |> ExMonty.Mount.add("/output", "/var/lib/myapp/output", :read_write,
       write_bytes_limit: 10_000_000)

ExMonty.Sandbox.run(code, os: mounts)
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
```

### Mount modes

| Atom         | Behaviour                                                           |
|--------------|---------------------------------------------------------------------|
| `:read_only` | Reads passthrough to host. Writes raise `PermissionError`.          |
| `:read_write`| Full passthrough. Writes hit the real disk. **Footgun — see §6.4.** |
| `:overlay`   | Reads fall through to host. Writes captured in-memory; host is untouched. Deletions create tombstones that hide real files for subsequent reads. |

### Module surface

```elixir
defmodule ExMonty.Mount do
  @opaque t :: %__MODULE__{ref: reference()}

  @type mode :: :read_only | :read_write | :overlay
  @type add_opts :: [write_bytes_limit: pos_integer()]

  @spec new() :: t()
  @spec add(t(), virtual :: String.t(), host :: String.t(), mode(), add_opts()) :: t()
  @spec count(t()) :: non_neg_integer()
  @spec list(t()) :: [%{virtual: String.t(), host: String.t(), mode: mode()}]
end
```

`add/5` returns the same `t()`; the underlying mount table is mutated in place
(it's a Rustler resource). Returning `t()` keeps the pipe-friendly API.

## Composition with existing `:os` options

The `:os` option already accepts an `ExMonty.PseudoFS{}` struct or a function
map. We add `ExMonty.Mount{}` as a third mutually-exclusive shape.

| `:os` value                      | Behaviour                                  |
|----------------------------------|--------------------------------------------|
| `%ExMonty.PseudoFS{}`            | (unchanged) in-memory virtual FS           |
| `%{atom => fn}`                  | (unchanged) per-function handler map       |
| **`%ExMonty.Mount{}`** *(new)*   | host directory mounts                      |

**Mutually exclusive in v1.** No combining `:mounts` with `:pseudo_fs` or with a
function map. Picked one model after weighing two alternatives:

- *Composition list* (`os: [mounts, pseudo_fs, getenv_handler]`, first match
  wins) — flexible but invites edge cases ("what does `[a, b, a]` mean?").
- *Tagged map* (`os: %{mounts: ..., fallback: %{...}}`) — explicit but adds a
  shape every doc/spec has to handle.

Mutual exclusion is the smallest change. We can revisit composition once we
have user demand for "mounts + a `getenv` handler in the same run." See §6.5.

## Lifecycle semantics

A `Mount` is **stateful** — particularly `:overlay`, where writes from one run
must be visible to the next. The Elixir struct holds a Rustler resource that
owns `Arc<Mutex<Option<Mount>>>` slots, mirroring upstream's
`take_shared_mounts` / `put_back_shared_mounts` pattern.

**Per-run flow:**
1. `Sandbox.run` borrows each mount's slot via `Mutex::lock` + `Option::take`.
2. Mounts are assembled into a `MountTable` and used for the run.
3. After the run completes (success or error), mounts are put back into their
   slots.
4. The `%ExMonty.Mount{}` struct passed in is now safe to reuse.

**Concurrent use surfaces a clear error.** If two runs try to take the same
mount, the second gets:

```elixir
{:error, %ExMonty.Exception{
  type: :runtime_error,
  message: "mount '/data' is already in use by another run"
}}
```

Users who want concurrent reads against the same host directory should create
two `Mount` objects pointing at the same host path — they're cheap.

## Implementation outline

### Rust side (`native/ex_monty/src/`)

New module `mounts.rs`:

```rust
pub struct MountResource {
    slots: Vec<Arc<Mutex<Option<Mount>>>>,
    // Metadata for `list/1` and error messages
    descriptors: Vec<MountDescriptor>,
}

#[rustler::resource_impl]
impl Resource for MountResource {}

#[rustler::nif]
fn mounts_new() -> ResourceArc<MountResource> { ... }

#[rustler::nif]
fn mounts_add(
    mounts: ResourceArc<MountResource>,
    virtual_path: String,
    host_path: String,
    mode: Atom,
    write_bytes_limit: Option<u64>,
) -> NifResult<ResourceArc<MountResource>> { ... }

#[rustler::nif]
fn mounts_list(mounts: ResourceArc<MountResource>) -> Vec<MountInfo> { ... }
```

Dispatch — the loop in `interactive.rs` becomes mount-aware. When
`RunProgress::OsCall` arrives:

```rust
if let Some(mount_table) = &mut state.mounts {
    if let Some(result) = mount_table.handle_os_call(call.function, &call.args, &call.kwargs) {
        // Mount handled it — resume immediately with the result
        return resume_with(call, result);
    }
}
// No mount matched, or non-FS op — fall through to Elixir
return os_call_to_elixir(call);
```

This means mount-handled FS ops never pay the Rust↔Elixir round-trip cost.

### Elixir side (`lib/ex_monty/`)

- `lib/ex_monty/mount.ex` — new module, ~80 lines.
- `lib/ex_monty/sandbox.ex` — `dispatch_os/4` learns to recognize
  `%ExMonty.Mount{}`, `normalize_os_handlers/1` accepts it, mode atoms added
  to type docs.
- `lib/ex_monty/native.ex` — three new NIF declarations.

### Tests

- `test/ex_monty/mount_test.exs` — read-only blocks writes; read-write hits
  disk; overlay isolates writes; concurrent-use error; longest-prefix routing
  (`/data/users` matches `/data/users` mount before `/data` mount); symlink
  escape blocked; per-mount write-bytes limit enforced.
- `test/ex_monty/sandbox_test.exs` — extend with mount-as-os-option dispatch,
  conflict with PseudoFS / function-map, propagation of mount errors.

## Read-write mode safety

`:read_write` lets sandbox code modify real host files. That's a real footgun.

**Decision: documented, unguarded.** Same posture as `File.write/2` in core
Elixir — if someone explicitly opts into `:read_write`, they meant it. Every
example in the docs uses `:read_only` or `:overlay`; `:read_write` only
appears with a "use with caution" callout.

Considered but rejected: renaming to `:dangerous_read_write` to force
visibility. ExMonty is positioned for executing **untrusted Python** — a user
wiring up `:read_write` has already accepted that posture. Renaming the atom
would be theatre, not safety.

## Out of scope for v1

These should be revisited only with concrete user demand:

1. **Mounts + PseudoFS in the same run.** Fine in theory but invites questions
   about precedence for paths like `/data/foo` if both layers claim it.
2. **Mounts + function-map non-FS handlers.** A user might want
   `os: %{mounts: ..., fallback: %{getenv: ...}}` to combine FS mounts with a
   custom `getenv`. This is the natural extension if anyone asks. Until then,
   you can write a handler module that delegates to mounts.
3. **Persistent overlay state.** Today's overlay is in-memory only. Saving and
   restoring overlay state across BEAM restarts is tractable but separate.
4. **Per-mount resource limits beyond `write_bytes_limit`.** Read-bytes limit,
   inode count, etc. — upstream doesn't expose them today.
5. **Network-style mounts** (HTTP-backed FS, S3, etc.) — not in upstream
   monty's scope.

## Open questions for reviewers

1. **API: `add/5` returns `t()`.** Pipe-friendly but slightly misleading since
   the resource is mutated in place. Alternatives: `add/5` returns `:ok` and
   we drop the chaining, or accept a list of mount specs in `new/1`. Pipe
   chaining feels more idiomatic, but I want to surface the trade-off.

2. **Mode atoms vs. tagged tuples.** `:overlay` is straightforward, but what
   about per-mount overlay configuration (max overlay size, persist-on-disk
   later)? Should the API be `{:overlay, opts}` from day one to leave room?
   I'd say no — YAGNI — but flagging.

3. **Concurrent-run error.** Currently surfaces as a generic
   `runtime_error`. Should this be a distinct exception type
   (`:mount_conflict_error`)? Cheap to add now, painful to retrofit.

4. **`list/1` field shape.** I have `%{virtual:, host:, mode:}` — should it
   also include `write_bytes_used / write_bytes_limit` for monitoring?

5. **Should `Sandbox.run` accept `mounts:` as a separate option?** I.e.
   `Sandbox.run(code, mounts: ..., os: pseudo_fs)`. Lets you compose mounts +
   PseudoFS in v1 after all, with mounts checked first. This is a real
   alternative to the mutual-exclusion stance in §3 — worth a hard look.

## Effort estimate

Roughly 1–2 days for a careful implementation:

- 0.5 day Rust side (resource + NIFs + dispatch wiring)
- 0.5 day Elixir side (`Mount` module + `Sandbox` integration)
- 0.5 day tests, including security tests (path escape, symlink escape)
- 0.5 day docs, examples, CHANGELOG

Multiply by 1.5 if §5 (composition with PseudoFS) lands in v1.

## References

- Upstream monty `fs::MountTable`: `crates/monty/src/fs/mount_table.rs`
- Upstream Python bindings (showing one wiring approach):
  `crates/monty-python/src/monty_cls.rs` (commit `6692c0b`)
- Upstream tests: `crates/monty-python/tests/test_mount_table.py`
