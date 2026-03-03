# Changelog

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
