# ExMonty

Elixir NIF wrapper for [Monty](https://github.com/pydantic/monty/), a minimal
secure Python interpreter written in Rust.

Execute Python code from Elixir with microsecond startup, full sandboxing,
resource limits, and interactive pause/resume for external function calls and
filesystem access.

## Features

- **Fast** --- no Python runtime required, microsecond startup
- **Safe** --- sandboxed execution with configurable memory, time, and recursion limits
- **Interactive** --- Python code pauses at external function calls, hands control to Elixir, and resumes with results
- **Pseudo filesystem** --- provide virtual files and environment variables to Python code without touching the real filesystem
- **Host filesystem mounts** --- map virtual sandbox paths to real host directories with read-only / read-write / overlay modes and symlink-escape protection
- **Natural type mapping** --- Python types map to Elixir types (dicts to maps, sets to MapSet, etc.)

## Installation

```elixir
def deps do
  [
    {:ex_monty, "~> 0.3"}
  ]
end
```

Requires Rust >= 1.90. Building also requires network access (the Monty Rust crate is a git dependency).

## Quick Start

### Simple Evaluation

```elixir
{:ok, 4, ""} = ExMonty.eval("2 + 2")

{:ok, result, ""} = ExMonty.eval("[x**2 for x in range(10)]")
# result = [0, 1, 4, 9, 16, 25, 36, 49, 64, 81]
```

### With Inputs

```elixir
{:ok, result, ""} = ExMonty.eval("x + y", inputs: %{"x" => 10, "y" => 20})
# result = 30
```

### Print Capture

All `print()` output is captured and returned as the third element:

```elixir
{:ok, nil, "hello world\n"} = ExMonty.eval("print('hello world')")
```

### Compile Once, Run Many

```elixir
{:ok, runner} = ExMonty.compile("x * 2", inputs: ["x"])

{:ok, 10, ""} = ExMonty.run(runner, %{"x" => 5})
{:ok, 20, ""} = ExMonty.run(runner, %{"x" => 10})
{:ok, 200, ""} = ExMonty.run(runner, %{"x" => 100})
```

## Interactive Execution

Monty's killer feature is interactive execution: Python code pauses when it
calls an external function, hands control back to Elixir, and resumes with
the result.

### Low-Level API

External functions are auto-detected at runtime — no upfront declaration needed.
When Python code calls an undefined function, execution pauses with a
`:function_call` progress tag. When code references an undefined name without
calling it, execution pauses with `:name_lookup`.

```elixir
{:ok, runner} = ExMonty.compile("result = fetch(url)\nresult", inputs: ["url"])

{:ok, progress} = ExMonty.start(runner, %{"url" => "https://example.com"})

case progress do
  {:function_call, call, snapshot, output} ->
    # call.name == "fetch", call.args == ["https://example.com"]
    response = do_fetch(call.args)
    {:ok, next} = ExMonty.resume(snapshot, {:ok, response})

  {:name_lookup, name, snapshot, output} ->
    # Provide a function object or value for the undefined name
    {:ok, next} = ExMonty.resume(snapshot, {:ok, {:function, name}})

  {:os_call, call, snapshot, output} ->
    # call.function == :read_text, call.args == [{:path, "/some/file"}]
    {:ok, next} = ExMonty.resume(snapshot, {:ok, file_content})

  {:complete, value, output} ->
    value
end
```

### High-Level Sandbox

`ExMonty.Sandbox` automates the interactive loop:

```elixir
{:ok, 42, ""} = ExMonty.Sandbox.run(
  "double(21)",
  functions: %{
    "double" => fn [x], _kwargs -> {:ok, x * 2} end
  }
)
```

With a handler module:

```elixir
defmodule MyHandler do
  @behaviour ExMonty.Sandbox

  @impl true
  def handle_function("fetch", [url], _kwargs) do
    case Req.get(url) do
      {:ok, resp} -> {:ok, resp.body}
      {:error, _} -> {:error, :runtime_error, "fetch failed"}
    end
  end
end

{:ok, result, _output} = ExMonty.Sandbox.run(code, handler: MyHandler)
```

## Pseudo Filesystem

Python code using `pathlib.Path` and the `os` module generates OS calls that
pause execution just like external function calls. `ExMonty.PseudoFS` provides
a complete in-memory virtual filesystem so Python code can read and write files
without touching the real filesystem.

```elixir
alias ExMonty.PseudoFS

fs = PseudoFS.new()
  |> PseudoFS.put_file("/data/config.json", ~s({"model": "gpt-4", "temperature": 0.7}))
  |> PseudoFS.put_file("/data/prompt.txt", "Summarize the following text:")
  |> PseudoFS.mkdir("/output")
  |> PseudoFS.put_env("API_KEY", "sk-secret123")

code = """
from pathlib import Path
import os

config = Path('/data/config.json').read_text()
prompt = Path('/data/prompt.txt').read_text()
api_key = os.getenv('API_KEY')

Path('/output/result.txt').write_text(f'Read config: {config}')
Path('/output/result.txt').read_text()
"""

{:ok, result, _output} = ExMonty.Sandbox.run(code, os: fs)
# result = "Read config: {\"model\": \"gpt-4\", \"temperature\": 0.7}"
```

### Supported Operations

| Python                    | OS Function   | Description                    |
|---------------------------|---------------|--------------------------------|
| `Path.exists()`           | `:exists`     | Check if path exists           |
| `Path.is_file()`          | `:is_file`    | Check if path is a file        |
| `Path.is_dir()`           | `:is_dir`     | Check if path is a directory   |
| `Path.is_symlink()`       | `:is_symlink` | Always returns `False`         |
| `Path.read_text()`        | `:read_text`  | Read file as string            |
| `Path.read_bytes()`       | `:read_bytes` | Read file as bytes             |
| `Path.write_text(data)`   | `:write_text` | Write string to file           |
| `Path.write_bytes(data)`  | `:write_bytes`| Write bytes to file            |
| append text               | `:append_text`| Append string to file          |
| append bytes              | `:append_bytes`| Append bytes to file          |
| `open(path, mode)`        | `:open`       | Open a file; args `[{:path, path}, mode]`, returns a `{:file_handle, ...}` |
| `Path.mkdir()`            | `:mkdir`      | Create directory               |
| `Path.unlink()`           | `:unlink`     | Delete file                    |
| `Path.rmdir()`            | `:rmdir`      | Delete empty directory         |
| `Path.iterdir()`          | `:iterdir`    | List directory contents        |
| `Path.stat()`             | `:stat`       | Get file metadata              |
| `Path.rename(target)`     | `:rename`     | Move/rename file               |
| `Path.resolve()`          | `:resolve`    | Get resolved path              |
| `Path.absolute()`         | `:absolute`   | Get absolute path              |
| `os.getenv(key)`          | `:getenv`     | Get environment variable       |
| `os.environ`              | `:get_environ`| Get all environment variables  |
| `date.today()`            | `:date_today` | Today's date from the host clock |
| `datetime.now(tz=...)`    | `:datetime_now` | Current datetime from the host clock; first arg is timezone or `nil` |

### Custom OS Handlers

For cases where PseudoFS isn't enough (e.g., proxying to the real filesystem
with access controls), implement `handle_os/3` in a handler module or pass a
function map:

```elixir
# Function map
{:ok, result, _} = ExMonty.Sandbox.run(code,
  os: %{
    read_text: fn [{:path, path}], _kwargs ->
      case File.read(path) do
        {:ok, content} -> {:ok, content}
        {:error, _} -> {:error, :file_not_found_error, "not found: #{path}"}
      end
    end
  }
)

# Handler module
defmodule MyOsHandler do
  @behaviour ExMonty.Sandbox

  @impl true
  def handle_function(_, _, _), do: {:error, :name_error, "not defined"}

  @impl true
  def handle_os(:read_text, [{:path, path}], _kwargs) do
    if String.starts_with?(path, "/allowed/") do
      case File.read(path) do
        {:ok, content} -> {:ok, content}
        {:error, _} -> {:error, :file_not_found_error, "not found"}
      end
    else
      {:error, :os_error, "access denied: #{path}"}
    end
  end
end
```

### Clock Handlers (`date.today()` / `datetime.now()`)

Python's `date.today()` and `datetime.now()` are surfaced as `:date_today` and
`:datetime_now` os calls. Provide handlers to control what "now" means — useful
for deterministic tests, time-travel, or pinning to a request timestamp:

```elixir
{:ok, result, _} = ExMonty.Sandbox.run(
  """
  from datetime import date, datetime, timezone
  d = date.today()
  dt = datetime.now(tz=timezone.utc)
  (d.year, dt.hour, dt.tzinfo is not None)
  """,
  os: %{
    date_today: fn _args, _kwargs ->
      {:ok, {:date, %{year: 2026, month: 5, day: 1}}}
    end,
    datetime_now: fn _args, _kwargs ->
      {:ok,
       {:datetime,
        %{
          year: 2026, month: 5, day: 1,
          hour: 14, minute: 30, second: 0, microsecond: 0,
          offset_seconds: 0, tz_name: nil
        }}}
    end
  }
)
# result = {2026, 14, true}
```

The handler's first argument for `:datetime_now` is the requested timezone
(`{:timezone, %{offset_seconds: ..., name: ...}}` for an aware datetime, or
`nil` for a naive one). If you don't care, ignore it; if you do, branch on it.

## Host Filesystem Mounts

`ExMonty.PseudoFS` is purely in-memory. When you need the sandbox to read
real host directories — but with sandboxing guarantees — use
`ExMonty.Mount`. A mount maps a virtual path inside the sandbox to a host
directory with one of three access modes. Path canonicalisation, boundary
checks, and symlink-escape detection are always enforced.

```elixir
mounts =
  ExMonty.Mount.new!()
  |> ExMonty.Mount.add!("/data",    "/var/lib/myapp/data",    :read_only)
  |> ExMonty.Mount.add!("/scratch", "/tmp/sandbox-scratch",   :overlay)
  |> ExMonty.Mount.add!("/output",  "/var/lib/myapp/output",  :read_write,
       write_bytes_limit: 10_000_000)

ExMonty.Sandbox.run(code, mounts: mounts)
```

### Modes

| Mode          | Reads               | Writes                                  |
|---------------|---------------------|-----------------------------------------|
| `:read_only`  | pass through to host | raise `PermissionError`                 |
| `:read_write` | pass through to host | hit the host disk **(footgun, see below)** |
| `:overlay`    | pass through to host | captured in-memory; host untouched      |

### Examples

**Read-only access to a data directory:**

```elixir
mounts = ExMonty.Mount.new!() |> ExMonty.Mount.add!("/data", "/var/lib/myapp/data", :read_only)

code = """
from pathlib import Path
Path("/data/users.csv").read_text()
"""

{:ok, csv, _} = ExMonty.Sandbox.run(code, mounts: mounts)
```

**Overlay (sandbox writes are ephemeral):**

```elixir
mounts = ExMonty.Mount.new!() |> ExMonty.Mount.add!("/scratch", "/tmp/work", :overlay)

# Sandbox writes go into in-memory overlay storage on the mount object,
# not to /tmp/work. The host directory stays untouched.
code = """
from pathlib import Path
Path("/scratch/intermediate.json").write_text("{}")
Path("/scratch/intermediate.json").read_text()
"""

{:ok, _, _} = ExMonty.Sandbox.run(code, mounts: mounts)
```

Overlay state **persists across runs against the same mount object**.
Construct a fresh mount to discard accumulated overlay writes.

**Compose mounts with `:os` fallbacks** (mounts handle FS calls; the
`:os` map handles non-FS calls like `getenv` and `datetime_now`):

```elixir
ExMonty.Sandbox.run(code,
  mounts: mounts,
  os: %{
    getenv:       fn _args, _kwargs -> {:ok, "from-host"} end,
    datetime_now: fn _args, _kwargs -> {:ok, fixed_datetime()} end
  }
)
```

### Unmounted paths

Filesystem operations on paths that don't fall under any mount raise
`PermissionError`:

```python
Path("/etc/passwd").read_text()
# PermissionError: Permission denied: '/etc/passwd'
```

This is upstream monty's `OsFunction::on_no_handler` behaviour for
filesystem operations. Non-filesystem operations (like `os.getenv`) raise
`RuntimeError` if no fallback handler is configured.

### Cumulative limits

`write_bytes_limit` is **cumulative on the mount object**, not per-run:

```elixir
mounts = ExMonty.Mount.new!() |> ExMonty.Mount.add!("/o", tmp, :overlay, write_bytes_limit: 16)

ExMonty.Sandbox.run(write_10_bytes, mounts: mounts)  # OK: 10/16 used
ExMonty.Sandbox.run(write_10_bytes, mounts: mounts)  # FAILS: 20 > 16
```

Construct a fresh mount to reset the counter.

### Concurrency

A mount can only serve one run at a time. Calling `Sandbox.run` against
a mount that's already in a run returns `{:error, :mount_in_use}`. If
you need parallel runs with the same host directory, create separate
mount objects.

### `:read_write` is a footgun

Sandbox code in `:read_write` mode can modify real host files. Use
sparingly. Most sandboxed-execution use cases want `:read_only` (provide
data) or `:overlay` (let the sandbox scribble freely without touching
disk).

## Resource Limits

Control memory, execution time, allocations, and recursion depth:

```elixir
{:ok, runner} = ExMonty.compile(code)

{:ok, result, output} = ExMonty.run(runner, %{}, limits: %{
  max_duration_secs: 5.0,       # wall-clock timeout
  max_memory: 10_000_000,       # ~10MB memory limit
  max_allocations: 100_000,     # heap allocation count limit
  max_recursion_depth: 100      # call stack depth limit
})
```

When a limit is exceeded, execution stops and an error is returned:

```elixir
{:error, %ExMonty.Exception{type: :recursion_error}} =
  ExMonty.eval("def f(): return f()\nf()", limits: %{max_recursion_depth: 50})
```

## Serialization

Runners and snapshots can be serialized to binary for storage or transfer:

```elixir
# Serialize a compiled runner
{:ok, runner} = ExMonty.compile("x + 1", inputs: ["x"])
{:ok, binary} = ExMonty.dump(runner)

# Restore and use
{:ok, restored} = ExMonty.load_runner(binary)
{:ok, 2, ""} = ExMonty.run(restored, %{"x" => 1})

# Serialize a paused snapshot (for long-running workflows)
{:ok, {:function_call, _call, snapshot, _}} = ExMonty.start(runner_with_ext_fns)
{:ok, snap_binary} = ExMonty.dump_snapshot(snapshot)

# Later: restore and resume
{:ok, restored_snap} = ExMonty.load_snapshot(snap_binary)
{:ok, {:complete, result, _}} = ExMonty.resume(restored_snap, {:ok, value})
```

## Python Support

Monty does **not** implement all of CPython. It targets a useful subset of the
language and standard library — enough to evaluate expressions, scripts, and
data transformations safely. This section calls out features that frequently
trip people up.

### Standard Library

```python
import math, json, re      # multi-module import works
math.pi                     # 3.14159...
json.dumps([1, 2, 3])       # '[1, 2, 3]'
json.loads('{"a": 1}')      # {'a': 1}  → Elixir %{"a" => 1}
re.match(r"\d+", "42")      # match object
```

Available modules: `math`, `json`, `re`, `os` (host-mediated), `pathlib`
(host-mediated), `datetime` (host-mediated for `today()` / `now()`).

### Syntax

```python
# Multi-module import
import a, b, c

# Chain assignment
a = b = c = 7

# Nested subscript assignment
d["k"][1] = 99
matrix[i][j] = 0

# Generalised unpacking (PEP 448)
[*a, *b]
{**defaults, **overrides}

# Augmented subscript assignment
counts[k] += 1
```

Class definitions are **not** supported — use `@dataclass` or pass objects in
from Elixir as `%ExMonty.Dataclass{}`. `hasattr` and `setattr` work on those
host-provided objects:

```python
hasattr(user, "email")     # works on dataclasses passed from Elixir
setattr(user, "name", "x") # works if dataclass is mutable (frozen=False)
```

### Built-ins

```python
zip([1,2,3], [4,5], strict=True)   # raises ValueError on length mismatch
"a\tb\tc".expandtabs(4)             # "a   b   c"
list(filter(None, items))           # filter, map, sorted, max, min, ...
```

### `int()` Parse Limits

CPython's `INT_MAX_STR_DIGITS` guard is enforced (default 4300 digits). This
prevents quadratic-time DoS via huge numeric strings:

```python
int("1" * 5000)
# ValueError: Exceeds the limit (4300 digits) for integer string conversion
```

## Type Mapping

| Python              | Elixir                          | Notes                                  |
|---------------------|---------------------------------|----------------------------------------|
| `None`              | `nil`                           |                                        |
| `True` / `False`    | `true` / `false`                |                                        |
| `int`               | `integer`                       | Arbitrary precision                    |
| `float`             | `float`                         |                                        |
| `str`               | `binary` (UTF-8)                |                                        |
| `bytes`             | `{:bytes, binary}`              | Tagged to distinguish from string      |
| `list`              | `list`                          |                                        |
| `tuple`             | `tuple`                         |                                        |
| `dict`              | `map`                           | Supports any key type                  |
| `set` / `frozenset` | `MapSet`                        |                                        |
| `...` (Ellipsis)    | `:ellipsis`                     |                                        |
| `Path`              | `{:path, string}`               |                                        |
| file object (`open()`) | `{:file_handle, %{path, mode, position}}` | `mode` is a Python mode string (`"r"`, `"wb"`, ...); produced by `open()` and accepted back from an `:open` os handler |
| `NamedTuple`        | `{:named_tuple, type_name, fields}` | `type_name` is a string; `fields` is an ordered list of `{field_name, value}` pairs |
| `@dataclass`        | `%ExMonty.Dataclass{}`          | `fields` keys are strings              |
| `datetime.date`     | `{:date, %{year, month, day}}`  | Output-only today                       |
| `datetime.datetime` | `{:datetime, %{year, month, day, hour, minute, second, microsecond, offset_seconds, tz_name}}` | `offset_seconds`/`tz_name` are `nil` for naive datetimes |
| `datetime.timedelta`| `{:timedelta, %{days, seconds, microseconds}}` |                                |
| `datetime.timezone` | `{:timezone, %{offset_seconds, name}}` | `name` may be `nil`                       |
| Exception           | `%ExMonty.Exception{}`          | With type, message, traceback          |

### Input Direction (Elixir to Python)

Native Elixir types are auto-detected. Use tagged tuples for ambiguous cases:

```elixir
# Automatic
ExMonty.eval("x", inputs: %{"x" => 42})         # int
ExMonty.eval("x", inputs: %{"x" => "hello"})     # str
ExMonty.eval("x", inputs: %{"x" => [1, 2, 3]})   # list
ExMonty.eval("x", inputs: %{"x" => %{"a" => 1}}) # dict

# Tagged
ExMonty.eval("x", inputs: %{"x" => {:bytes, <<1, 2, 3>>}}) # bytes
ExMonty.eval("x", inputs: %{"x" => {:path, "/tmp/file"}})   # Path

# datetime types — same shape on input and output
ExMonty.eval("x.year", inputs: %{"x" => {:date, %{year: 2026, month: 5, day: 1}}})
# {:ok, 2026, ""}

ExMonty.eval("x.tzinfo is not None",
  inputs: %{"x" => {:datetime, %{
    year: 2026, month: 5, day: 1,
    hour: 12, minute: 0, second: 0, microsecond: 0,
    offset_seconds: 0, tz_name: nil
  }}})
# {:ok, true, ""}

ExMonty.eval("x.days", inputs: %{"x" => {:timedelta, %{days: 7, seconds: 0, microseconds: 0}}})
# {:ok, 7, ""}
```

## Error Handling

Errors are returned as `{:error, %ExMonty.Exception{}}`:

```elixir
{:error, %ExMonty.Exception{
  type: :zero_division_error,
  message: "division by zero",
  traceback: [%ExMonty.StackFrame{filename: "main.py", line: 1, ...}]
}} = ExMonty.eval("1 / 0")
```

Exception types are atoms matching Python exception names in snake_case:
`:value_error`, `:type_error`, `:key_error`, `:index_error`,
`:name_error`, `:attribute_error`, `:runtime_error`, `:syntax_error`,
`:file_not_found_error`, `:zero_division_error`, `:recursion_error`, etc.

## Architecture

```
ExMonty (Elixir API)
  |
  +-- ExMonty.Sandbox (handler behaviour, interactive loop)
  |     +-- ExMonty.PseudoFS (in-memory virtual filesystem)
  |     +-- ExMonty.Mount (host filesystem mounts with sandboxing)
  |
  +-- ExMonty.Native (NIF bindings via Rustler)
        |
        +-- Rust NIF crate (type conversion, resource management)
              |
              +-- monty crate (Python interpreter; fs::MountTable for mounts)
```

## Releasing

Releases are automated. Pushing a `vX.Y.Z` tag builds the precompiled NIFs,
creates a GitHub release, and publishes to Hex — **pausing for a manual approval
before anything ships**. You never hand-build checksums or re-tag.

**One-time setup.** Hex no longer mints API keys from the CLI (auth is OAuth);
generate one at [hex.pm/dashboard/keys](https://hex.pm/dashboard/keys) with the
`api` permission, then store it scoped to the `hex` environment:

```bash
gh secret set HEX_API_KEY --env hex --repo jtippett/ex_monty
```

**To cut a release:**

1. Bump `@version` in `mix.exs`; move the `CHANGELOG.md` `[Unreleased]` section
   under the new version number.
2. Open a PR and merge to `master` once CI is green.
3. Tag the merge commit and push the tag:
   ```bash
   git tag -a vX.Y.Z -m vX.Y.Z && git push origin vX.Y.Z
   ```
4. `release.yml` builds NIFs for all four targets, creates the GitHub release
   with the `.tar.gz` artifacts, then **waits for approval**.
5. Review the release, then approve the **`hex`** deployment in the workflow run
   (Actions → the run → *Review deployments* → approve). On approval it
   regenerates `checksum-Elixir.ExMonty.Native.exs` from the released artifacts
   and runs `mix hex.publish`.

Don't commit the checksum file or move a published tag by hand — the pipeline
owns both. See [`UPDATE_PROCEDURE.md`](UPDATE_PROCEDURE.md) for bumping the
pinned monty version.

## License

MIT
