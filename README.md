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
- **Natural type mapping** --- Python types map to Elixir types (dicts to maps, sets to MapSet, etc.)

## Installation

```elixir
def deps do
  [
    {:ex_monty, "~> 0.1.0"}
  ]
end
```

Requires Rust >= 1.85 (for the monty crate's edition 2024).

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

```elixir
{:ok, runner} = ExMonty.compile(
  "result = fetch(url)\nresult",
  inputs: ["url"],
  external_functions: ["fetch"]
)

{:ok, progress} = ExMonty.start(runner, %{"url" => "https://example.com"})

case progress do
  {:function_call, call, snapshot, output} ->
    # call.name == "fetch", call.args == ["https://example.com"]
    response = do_fetch(call.args)
    {:ok, next} = ExMonty.resume(snapshot, {:ok, response})

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

{:ok, result, _output} = ExMonty.Sandbox.run(code,
  handler: MyHandler,
  external_functions: ["fetch"]
)
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
| `NamedTuple`        | `{atom, %{fields}}`             | Type name as snake_case atom           |
| `@dataclass`        | `%ExMonty.Dataclass{}`          |                                        |
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
  |
  +-- ExMonty.Native (NIF bindings via Rustler)
        |
        +-- Rust NIF crate (type conversion, resource management)
              |
              +-- monty crate (Python interpreter)
```

## License

MIT
