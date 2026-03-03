# ExMonty Update Procedure

Run this procedure periodically to pull upstream monty changes and update ExMonty.

## Overview

ExMonty pins a specific monty git revision in `native/ex_monty/Cargo.toml`. Upstream
monty is under active development (~200+ commits since Jan 2025). This procedure walks
through pulling, assessing, and integrating changes.

---

## Phase 1: Pull and Assess

### 1.1 Fetch latest upstream

```bash
cd ../monty && git fetch origin && git log --oneline HEAD..origin/main
```

(The monty repo is at `../monty` relative to the ex_monty project root.)

If you maintain a local branch, also:

```bash
git pull origin main
```

### 1.2 Identify our current pin

```bash
grep 'rev = ' native/ex_monty/Cargo.toml
```

### 1.3 Review changes since our pin

This is the critical step. Focus on the public API surface.

```bash
cd ../monty
git diff <OUR_PINNED_REV>..origin/main -- crates/monty/src/lib.rs
```

This file contains **all** public re-exports. Any change here directly affects us.
Look for:

- **Renamed types or functions** — will cause compile errors
- **Changed signatures** (new params, different return types) — silent breakage risk
- **Removed exports** — will cause compile errors
- **New exports** — opportunities for new features

Then review the implementation files behind our dependencies:

```bash
# Our full API surface — review changes in these modules:
git diff <OUR_PINNED_REV>..origin/main -- \
  crates/monty/src/run.rs \
  crates/monty/src/object.rs \
  crates/monty/src/os.rs \
  crates/monty/src/resource.rs \
  crates/monty/src/exception_public.rs \
  crates/monty/src/exception_private.rs \
  crates/monty/src/io.rs
```

### 1.4 Read the commit log for context

```bash
git log --oneline <OUR_PINNED_REV>..origin/main -- crates/monty/
```

Pay attention to commits that mention: breaking changes, API changes, new features,
bug fixes, or anything related to types we use.

---

## Phase 2: Classify Changes

Sort what you found into categories:

### Breaking (must fix before bump)
Changes to types/functions we directly use:

| We use | Where in our code |
|--------|-------------------|
| `MontyRun::new()`, `.run()`, `.start()`, `.clone()` | `lib.rs`, `interactive.rs` |
| `MontyObject` variants + encoding | `types.rs` (22KB — largest module) |
| `OsFunction` variants | `types.rs`, `interactive.rs` |
| `MontyException`, `ExcType`, `StackFrame` | `error.rs`, `interactive.rs` |
| `Snapshot::run()`, `FutureSnapshot::resume()` | `interactive.rs`, `resources.rs` |
| `RunProgress` variants | `interactive.rs` |
| `ExternalResult` variants | `interactive.rs` |
| `CollectStringPrint`, `.into_output()` | `lib.rs` |
| `ResourceLimits`, `LimitedTracker`, `ResourceError` | `lib.rs`, `types.rs`, `resources.rs` |
| `Serialize`/`Deserialize` on `MontyRun`, `Snapshot`, `FutureSnapshot` | `serialization.rs` |

### Non-breaking improvements
- Bug fixes in monty internals (no API change)
- New `MontyObject` variants (we'll hit a match-not-exhaustive warning)
- New `OsFunction` variants (same)
- Performance improvements

### New capabilities
- New public API types/functions we might want to expose

---

## Phase 3: Update

### 3.1 Switch to path dependency for development

Edit `native/ex_monty/Cargo.toml`:

```toml
# Comment out the git dep:
# monty = { git = "https://github.com/pydantic/monty.git", rev = "..." }

# Use local path for iteration:
monty = { path = "../../../monty/crates/monty" }
```

(Assumes `../monty` relative to `ex_monty` project root, i.e. `../../../monty/crates/monty`
relative to the Cargo.toml.)

### 3.2 Build and fix compile errors

```bash
cd native/ex_monty && cargo check 2>&1
```

Common fixes by category:

**Renamed types/functions** — Find-and-replace in our Rust source.

**Changed method signatures** — Update call sites in our NIFs. Check if new
parameters need to be threaded through from Elixir.

**New MontyObject variants** — Add arms to the match in `types.rs:encode_monty_object()`.
Decide on Elixir encoding (tagged tuple, struct, etc.).

**New OsFunction variants** — Add arms to `encode_os_function()` in `types.rs`
and handling in `interactive.rs`.

**New ExcType variants** — Usually handled automatically by `ExcType::from_str()`,
but verify.

**Serialization breakage** — If `MontyRun`, `Snapshot`, or `FutureSnapshot` changed
their serialized format, existing serialized data is incompatible. Consider bumping
a version marker if we persist these.

### 3.3 Update Elixir side if needed

If new types or call patterns were added, update:
- `lib/ex_monty.ex` — main API
- `lib/ex_monty/native.ex` — NIF bindings
- `lib/ex_monty/sandbox.ex` — handler dispatch
- Relevant structs in `lib/ex_monty/`

### 3.4 Run the test suite

```bash
mix test
```

All 90+ tests should pass. If upstream changed behavior (not just API), some
assertions may need updating.

### 3.5 Add tests for new functionality

If new MontyObject variants or OsFunction variants were added, write tests
exercising them through `ExMonty.eval/2` or the sandbox.

---

## Phase 4: Pin and Ship

### 4.1 Switch back to git dependency

```toml
monty = { git = "https://github.com/pydantic/monty.git", rev = "<NEW_COMMIT_HASH>" }
```

Use the full 40-char hash of the monty commit you tested against.

### 4.2 Verify clean build from git dep

```bash
cd native/ex_monty && cargo update -p monty
cd ../.. && mix clean && mix test
```

### 4.3 Update CHANGELOG.md

If the update includes breaking changes, new features, or notable bug fixes,
add an entry to `CHANGELOG.md` under the `[Unreleased]` section summarizing
what changed from the ExMonty user's perspective.

### 4.4 Commit

```
Update monty to <short-hash>

- <summary of breaking changes fixed>
- <summary of new features added>
- <summary of upstream improvements picked up>
```

---

## Quick Reference: One-liner Diff Check

To quickly check if there are API changes worth pulling:

```bash
cd ../monty && git fetch origin && git diff $(grep -oP 'rev = "\K[^"]+' ../ex_monty/native/ex_monty/Cargo.toml)..origin/main -- crates/monty/src/lib.rs
```

If this diff is empty, there are no public API changes — only internal improvements.
Still worth updating periodically for bug fixes.

---

## When to Update

- **Immediately** if upstream fixes a bug affecting us
- **Weekly-ish** during active monty development
- **Before any ExMonty release** to pick up latest fixes
- **When a new MontyObject/OsFunction variant** is needed for a feature we want
