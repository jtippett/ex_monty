# Changelog

## 0.5.1 - 2026-07-15

### Security / Hardening

- Captured `print()` output is now fallibly allocated (via `try_reserve_exact`,
  so the buffer capacity cannot grow past the budget) and counted cumulatively
  against `max_memory`, including across interactive and serialized snapshots.
  This budget is tracked independently of Monty's heap tracker, so the effective
  peak for a captured run is bounded by roughly `2 × max_memory` (the printed
  value on Monty's heap plus the captured copy), not `max_memory` exactly.
- `Sandbox` callbacks run in monitored worker processes with a validated 10s
  default timeout. Raises, throws, exits, brutal kills, and timeouts no longer
  kill or indefinitely block the sandbox caller; timed-out workers are killed
  before control returns. This contains ordinary BEAM callbacks; it cannot
  cancel a non-cooperative native NIF a callback itself invokes (e.g. a nested
  `run` busy-loop on a dirty scheduler) or unlinked processes a
  callback spawns. Restrict callbacks to cooperative code.
- Callback, name-lookup, file-handle, bigint, dataclass, and future-result
  decoding is strict. Malformed replies, unknown exception types, duplicate or
  unknown future IDs, oversized ordinary bigints, and inconsistent dataclass
  fields are rejected before consuming resumable state.
- Potentially blocking mount release now runs on a dirty-I/O scheduler. Large
  mount/future metadata encoding uses dirty-CPU schedulers, and mount metadata
  locks are no longer held during host-path canonicalisation.
- Snapshot and future-snapshot dumps carry a versioned magic header and reject
  trailing bytes after a valid payload, so corrupt or appended data fails closed.

### Fixes

- `:pending` now exposes Monty's asynchronous external-call lifecycle through
  `resume/2`, making `resolve_futures`, `pending_call_ids/1`, and
  `resume_futures/2` reachable and retry-safe.
- `PseudoFS` now implements `:open`, `:append_text`, and `:append_bytes`, and a
  newly constructed filesystem contains its root directory.
- Public wrappers normalize Rustler `badarg` failures into `{:error, reason}`
  instead of leaking `ArgumentError` despite their return types.
- `Sandbox.run/2` no longer fabricates `nil` results if pending futures somehow
  reach its synchronous loop; it fails explicitly and directs callers to the
  low-level future API.
- A raising, timed-out, or error-returning `handle_name_lookup/1` now surfaces
  its real reason from `Sandbox.run/2` instead of the opaque strict-decode
  failure that resuming a name lookup with an error result would otherwise emit.

Snapshot and future-snapshot dumps created before this change are not compatible
with the new versioned, cumulative-output-budget serialization format. Runner
dumps are unchanged. The serialization APIs remain trusted-input-only.

## 0.5.0 - 2026-06-26

### Security / Hardening

This release hardens the NIF boundary against inputs that could previously
panic, exhaust memory, or **stack-overflow the BEAM VM** (a Rust crash in a NIF
takes down the whole node, not just one process). ExMonty runs untrusted code,
so the boundary is now defensive by default.

- **Default resource limits are now applied when `:limits` is omitted.**
  `run/3`, `eval/2`, `start/*`, and friends now default to conservative caps
  (10s wall-clock, 100M allocations, 512 MB memory, recursion depth 128) instead
  of running unbounded. Pass `limits: :unlimited` to opt out for trusted code, or
  a map to override individual fields. Inspect the defaults via
  `ExMonty.default_limits/0`. **This is a behavioral change**: code that
  previously relied on unbounded execution must now pass `limits: :unlimited`.

- **Recursion-depth cap is enforced in the NIF**, not just in Elixir, so it
  cannot be bypassed by calling `ExMonty.Native.*` directly. `max_recursion_depth`
  is clamped to a safe ceiling regardless of how the runner is invoked; see
  `ExMonty.max_recursion_depth_cap/0`.

- **Value encode/decode at the NIF boundary is depth-guarded.** Deeply nested
  inputs or result values are rejected with a clean error instead of overflowing
  the dirty-scheduler stack (which is uncatchable and crashes the VM).

- **Stricter decoding of tagged values.** Bigint tuples must carry a valid,
  canonical sign and a bounded magnitude; dataclass tuples validate `frozen`,
  `type_id`, and `field_names`; malformed resource-limit values are rejected
  rather than silently coerced. Negative or non-finite durations/limits error
  cleanly.

- **`PseudoFS` virtual paths are lexically normalized** (`.`, `..`, and
  duplicate slashes collapse; `..` cannot escape above the root) at every
  ingress.

- Snapshot resume peeks the callback result before consuming the snapshot, and
  mount-aware NIFs run on dirty-IO schedulers. Lock poisoning is recovered
  rather than propagated as a panic.

- **Known limitation:** `load_runner/1`, `load_snapshot/1`, and
  `load_future_snapshot/1` deserialize trusted bytes only — feeding them
  attacker-controlled binaries can still OOM or overflow. Documented inline.

## 0.4.2 - 2026-06-20

## 0.4.1 - 2026-06-20

## 0.4.0

### New ExMonty Features

- **Buffered `open()` builtin and file handles.** Python `open(path, mode)`
  (with context-manager `with` support, `read`/`write`, and `a`/`w`/`r`
  modes) now works against `:read_write` and `:overlay` mounts. File objects
  round-trip as a new `{:file_handle, %{path, mode, position}}` tagged value,
  and three new os calls — `:open`, `:append_text`, `:append_bytes` — are
  dispatchable through `Sandbox` `os:` handler maps. `PseudoFS` does not yet
  service `:open` (returns `NotImplementedError`); use a mount for file I/O.

- **`ExMonty.Mount` — host filesystem mounts.** Map virtual paths in the
  sandbox to real host directories with `:read_only`, `:read_write`, or
  `:overlay` access modes. Path canonicalisation, boundary checks, and
  symlink-escape detection are always enforced.

  ```elixir
  mounts =
    ExMonty.Mount.new!()
    |> ExMonty.Mount.add!("/data",    "/var/lib/myapp/data", :read_only)
    |> ExMonty.Mount.add!("/scratch", "/tmp/sandbox-scratch", :overlay)

  ExMonty.Sandbox.run(code, mounts: mounts)
  ```

  Composes with the existing `:os` option — mounts handle FS calls; the
  `:os` map handles non-FS fallbacks (`:getenv`, `:datetime_now`, etc.).
  Unmounted paths raise `PermissionError` (upstream's
  `OsFunction::on_no_handler` semantics) instead of ExMonty's previous
  generic `:os_error`.

  Mount state (overlay writes, `write_bytes_used` counter) is cumulative
  on the mount object across runs. Construct a fresh mount to discard.
  See `proposals/MOUNT_TABLE.md` for the design rationale.

- **`datetime` support.** `date`, `datetime`, `timedelta`, and `timezone`
  Python values round-trip via new tagged tuples:

  ```elixir
  # Output direction (Python → Elixir)
  ExMonty.eval("from datetime import date\ndate(2026, 5, 1)")
  # {:ok, {:date, %{year: 2026, month: 5, day: 1}}, ""}

  # Input direction (Elixir → Python)
  ExMonty.eval("x.year",
    inputs: %{"x" => {:date, %{year: 2026, month: 5, day: 1}}})
  # {:ok, 2026, ""}

  # date.today() / datetime.now() via host handlers
  ExMonty.Sandbox.run("from datetime import date\ndate.today()",
    os: %{date_today: fn _, _ -> {:ok, {:date, %{year: 2026, month: 5, day: 1}}} end})
  ```

  Encoded shapes:
  - `{:date, %{year, month, day}}`
  - `{:datetime, %{year, month, day, hour, minute, second, microsecond, offset_seconds, tz_name}}`
    (`offset_seconds` and `tz_name` are `nil` for naive datetimes)
  - `{:timedelta, %{days, seconds, microseconds}}`
  - `{:timezone, %{offset_seconds, name}}`

- **New OS handler atoms** `:date_today` and `:datetime_now` surfaced through
  `ExMonty.Sandbox` for hosts to provide deterministic clocks.

### Bug Fixes

- **Sandbox handler allowlist.** `ExMonty.Sandbox` now accepts `:date_today` /
  `:datetime_now` keys in `os:` handler maps (previously they were silently
  dropped).

### Upstream Python Features (monty v0.0.8 → v0.0.18)

These come from upstream and don't change the ExMonty API — but they affect
what Python code you can run:

- **`open()` builtin** (v0.0.18). `with open(...) as f:`, buffered read/write,
  file objects. The `with` statement works for built-in context managers like
  `open()`; user-defined `__enter__`/`__exit__` classes are still unsupported
  (no class definitions). See "Buffered `open()`" above.
- **`json` module.** `import json; json.loads(...) / json.dumps(...)`.
- **`datetime` module.** `date`, `datetime`, `timedelta`, `timezone` (see above).
- **Multi-module imports.** `import a, b, c` in one statement.
- **Chain assignment.** `a = b = c = 1`.
- **Nested subscript assignment.** `d[k][i] = v`, `matrix[i][j] = 0`.
- **`hasattr` / `setattr`.** Work on host-provided objects (dataclasses, modules).
  Class definitions in Python source are still not supported.
- **`zip(..., strict=True)`.** Raises `ValueError` on length mismatch.
- **`str.expandtabs(tabsize)`.**
- **Named single-kwarg calls.** `f(x=1)` (single-kwarg edge case fixed).
- **`INT_MAX_STR_DIGITS` guard.** `int("1" * 5000)` raises `ValueError`,
  matching CPython's quadratic-time DoS protection (default 4300 digits).
- **Bug fixes:** `i64::MIN` negation panic, source-line >65535 parse panic,
  partial future-resolution panic in mixed gathers, GC interval ignored in
  tracker, GC reference release in cycles, empty-tuple singleton counted
  against memory limit.
- **More hardening** (v0.0.18): trial-deletion cycle GC, duplicate-parameter
  and duplicate-coroutine panics, exception-stack corruption on `raise`,
  further integer-op panics, comprehension generator/AST depth limits, and
  JSON BigInt limits — all converted from panics to proper exceptions or
  bounded errors.

### Not Yet Exposed to Elixir

- **Filesystem mounting** (`MountTable` API) — upstream supports overlay /
  read-only / layered filesystems. ExMonty's `PseudoFS` does not yet wrap this.
- **Async in Rust** — internal VM async support; no surface change for Elixir
  callers today.
- **`MontyRepl` / `JsonMontyObject`** — Rust-only APIs.

### Internal

- **monty pinned to v0.0.18** (commit `45a3b2d5`). Update procedure now tracks
  tagged releases by default; see `UPDATE_PROCEDURE.md`.
- **Upstream `OsFunction` → `OsFunctionCall` refactor absorbed.** The os-call
  surface now carries typed args inline; our NIF extracts the
  `(positional, keyword)` view via `OsFunctionCall::to_args` and routes mount
  calls through the new `MountTable::handle_os_call(&call)` signature. No
  Elixir-side API change beyond the new `:open`/`:append_*` os atoms.
- **`PrintWriter::Collect` renamed to `PrintWriter::CollectString`** in monty
  upstream — internal NIF change only, no Elixir-side impact.

## 0.3.0

### Breaking Changes

- **Removed `external_functions` option from `compile/2` and `Sandbox.run/2`.**
  External functions are now auto-detected at runtime via name lookup. The
  `:external_functions` option is no longer accepted.
- **`ExMonty.Native.compile/4` is now `compile/3`** (removed `external_fns` parameter).

### New Features

- **Name lookup progress tag.** When Python code references an undefined name
  (without calling it), execution pauses with `{:name_lookup, name, snapshot, output}`.
  Resume with `{:ok, {:function, name}}` to provide a callable, `{:ok, value}` for a
  constant, or `:undefined` to raise `NameError`.
- **`MontyObject::Function` type.** A new tagged tuple `{:function, name}` (or
  `{:function, name, docstring}`) can be returned from name lookups to provide
  callable function objects to the Python VM.
- **`handle_name_lookup/1` callback** in `ExMonty.Sandbox` behaviour (optional).
  Called when the sandbox encounters an undefined name. The sandbox auto-resolves
  names found in the `:functions` map.

### Upstream Improvements (monty v0.0.8)

- `re` module implementation (regex support).
- Full Python `math` module (~50 functions).
- PEP 448 generalised unpacking (`[*a, *b]`, `{**a, **b}`).
- Tuple comparison operators (`<`, `>`, `<=`, `>=`).
- Dict view and set/frozenset operators.
- `str` and `bytes` comparison operators.
- Augmented assignment on subscript targets (`x[i] += 1`).
- `max()` kwargs/default support.
- Bug fixes: stack-depth tracking, loop iterator depth, variable shadowing panic,
  `os.environ` panic in JS bindings.

## 0.2.1

- Fix dataclass round-trip: `ExMonty.Dataclass` structs returned from handlers are now decoded back to `MontyObject::Dataclass`, preserving field access (`p.x`), frozen semantics, and method calls.
- Add `field_names` and `type_id` fields to `ExMonty.Dataclass` struct for full fidelity with monty's dataclass representation.
- Fix float inf/nan encoding: `float('inf')`, `float('-inf')`, and `float('nan')` now encode as `:infinity`, `:neg_infinity`, and `:nan` atoms instead of crashing.
- Float special atoms round-trip through inputs.

## 0.2.0

- Update monty to 47427c0 (v0.0.7).
- Support dataclass method calls (new `:method_call` tag in interactive mode).
- Upstream: `filter()`, `map()`, `getattr()` builtins, `dict(iterable)`, dataclass methods, `asyncio.run()`, user-defined function calling.
- Upstream: `MontyException` and `StackFrame` now serializable.
- Upstream: improved resource limit enforcement (time check rate-limiting, 4x pow safety multiplier, catchable RecursionError).
- Upstream: bug fixes for string comparison, async stack overflow, i64::MIN division overflow.
- PrintWriter refactored from trait to enum (internal change, no public API impact).

## 0.1.1

- Update monty to v0.0.4 (86712b6) — includes fuzz testing, small-tuple optimizations, AST depth overflow fix, and `id()` memory leak fix.

## 0.1.0

- Initial release.
- `ExMonty.eval/2`, `compile/2`, `run/3` for sandboxed Python execution.
- Interactive pause/resume (`start/3`, `resume/2`, `resume_futures/2`) for external function calls and OS calls.
- `ExMonty.Sandbox` high-level handler + `ExMonty.PseudoFS` in-memory filesystem.
- Runner/snapshot serialization (`dump/1`, `load_runner/1`, `dump_snapshot/1`, `load_snapshot/1`).
