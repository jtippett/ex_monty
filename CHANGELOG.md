# Changelog

## 0.1.0

- Initial release.
- `ExMonty.eval/2`, `compile/2`, `run/3` for sandboxed Python execution.
- Interactive pause/resume (`start/3`, `resume/2`, `resume_futures/2`) for external function calls and OS calls.
- `ExMonty.Sandbox` high-level handler + `ExMonty.PseudoFS` in-memory filesystem.
- Runner/snapshot serialization (`dump/1`, `load_runner/1`, `dump_snapshot/1`, `load_snapshot/1`).
