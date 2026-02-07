defmodule ExMonty do
  @moduledoc """
  Elixir wrapper for [Monty](https://github.com/pydantic/monty/), a minimal secure
  Python interpreter written in Rust.

  ExMonty provides safe execution of Python code from Elixir with:

    * **Microsecond startup** — no Python runtime needed
    * **Interactive execution** — code pauses at external function calls, hands
      control to Elixir, and resumes with results
    * **Resource limits** — control memory, CPU time, and recursion depth
    * **Full type mapping** — Python types map naturally to Elixir types

  ## Quick Start

      # Simple evaluation
      {:ok, result, output} = ExMonty.eval("2 + 2")
      # result = 4, output = ""

      # With inputs
      {:ok, runner} = ExMonty.compile("result = x + y", inputs: ["x", "y"])
      {:ok, result, output} = ExMonty.run(runner, %{"x" => 10, "y" => 20})
      # result = 30

  ## Interactive Execution

      {:ok, runner} = ExMonty.compile("result = fetch(url)",
        inputs: ["url"],
        external_functions: ["fetch"]
      )

      {:ok, progress} = ExMonty.start(runner, %{"url" => "https://example.com"})

      case progress do
        {:function_call, call, snapshot, output} ->
          response = do_fetch(call.name, call.args)
          {:ok, next} = ExMonty.resume(snapshot, {:ok, response})

        {:complete, value, output} ->
          value
      end

  See `ExMonty.Sandbox` for a high-level handler that automates the interactive loop.
  """

  alias ExMonty.Native

  @type runner :: reference()
  @type snapshot :: reference()
  @type future_snapshot :: reference()
  @type error_reason :: term()

  @type limits :: %{
          optional(:max_allocations) => non_neg_integer(),
          optional(:max_duration_secs) => float(),
          optional(:max_memory) => non_neg_integer(),
          optional(:gc_interval) => non_neg_integer(),
          optional(:max_recursion_depth) => non_neg_integer()
        }

  @type progress ::
          {:function_call, ExMonty.FunctionCall.t(), snapshot(), String.t()}
          | {:os_call, ExMonty.OsCall.t(), snapshot(), String.t()}
          | {:resolve_futures, future_snapshot(), String.t()}
          | {:complete, term(), String.t()}

  @doc """
  Compiles Python code into a reusable runner.

  The runner can be executed multiple times with different inputs via `run/3` or `start/3`.

  ## Options

    * `:inputs` - list of input variable names (default: `[]`)
    * `:external_functions` - list of external function names that will pause execution (default: `[]`)
    * `:script_name` - name for the script in tracebacks (default: `"main.py"`)

  ## Examples

      {:ok, runner} = ExMonty.compile("result = x * 2", inputs: ["x"])

      {:ok, runner} = ExMonty.compile("result = fetch(url)",
        inputs: ["url"],
        external_functions: ["fetch"]
      )
  """
  @spec compile(String.t(), keyword()) :: {:ok, runner()} | {:error, error_reason()}
  def compile(code, opts \\ []) do
    inputs = opts |> Keyword.get(:inputs, []) |> Enum.map(&to_string/1)
    external_fns = opts |> Keyword.get(:external_functions, []) |> Enum.map(&to_string/1)
    script_name = opts |> Keyword.get(:script_name, "main.py") |> to_string()

    with :ok <- validate_name_list("inputs", inputs),
         :ok <- validate_name_list("external_functions", external_fns) do
      inputs = Enum.sort(inputs)
      external_fns = Enum.sort(external_fns)

      case Native.compile(code, script_name, inputs, external_fns) do
        {:ok, runner} -> {:ok, runner}
        {:error, reason} -> {:error, reason}
        runner when is_reference(runner) -> {:ok, runner}
      end
    end
  end

  @doc """
  Runs a compiled runner to completion with the given inputs.

  Returns the result value and any captured print output.

  ## Options

    * `:limits` - resource limits map (default: `nil` for default limits)

  ## Examples

      {:ok, runner} = ExMonty.compile("result = x + y", inputs: ["x", "y"])
      {:ok, result, output} = ExMonty.run(runner, %{"x" => 1, "y" => 2})
      # result = 3, output = ""
  """
  @spec run(runner(), map(), keyword()) ::
          {:ok, term(), String.t()} | {:error, error_reason()}
  def run(runner, inputs \\ %{}, opts \\ []) do
    limits = Keyword.get(opts, :limits, nil)
    input_list = Enum.map(inputs, fn {k, v} -> {to_string(k), v} end)

    case Native.run(runner, input_list, limits) do
      {:error, reason} -> {:error, reason}
      {:ok, {result, output}} -> {:ok, result, output}
      {result, output} when is_binary(output) -> {:ok, result, output}
    end
  rescue
    e in ErlangError ->
      {:error, e.original}
  end

  @doc """
  Compiles and runs Python code in one call.

  Convenience function that combines `compile/2` and `run/3`.

  ## Options

    * `:inputs` - map of input variable names to values (default: `%{}`)
    * `:limits` - resource limits map (default: `nil`)
    * `:script_name` - script name for tracebacks (default: `"main.py"`)

  ## Examples

      {:ok, 4, ""} = ExMonty.eval("result = 2 + 2")

      {:ok, 30, ""} = ExMonty.eval("result = x + y",
        inputs: %{"x" => 10, "y" => 20}
      )
  """
  @spec eval(String.t(), keyword()) :: {:ok, term(), String.t()} | {:error, error_reason()}
  def eval(code, opts \\ []) do
    inputs = Keyword.get(opts, :inputs, %{})
    input_names = inputs |> Map.keys() |> Enum.map(&to_string/1) |> Enum.sort()
    limits = Keyword.get(opts, :limits, nil)
    script_name = Keyword.get(opts, :script_name, "main.py")

    compile_opts = [
      inputs: input_names,
      external_functions: Keyword.get(opts, :external_functions, []),
      script_name: script_name
    ]

    with {:ok, runner} <- compile(code, compile_opts) do
      run(runner, inputs, limits: limits)
    end
  end

  @doc """
  Starts interactive execution of a compiled runner.

  Returns a progress tuple that indicates the current state of execution.
  Use pattern matching to handle function calls, OS calls, futures, or completion.

  ## Options

    * `:limits` - resource limits map (default: `nil`)

  ## Progress Values

    * `{:function_call, %ExMonty.FunctionCall{}, snapshot, output}` — paused at external function call
    * `{:os_call, %ExMonty.OsCall{}, snapshot, output}` — paused at OS/filesystem operation
    * `{:resolve_futures, future_snapshot, output}` — paused waiting for async futures
    * `{:complete, value, output}` — execution finished

  ## Examples

      {:ok, runner} = ExMonty.compile("result = fetch(url)",
        inputs: ["url"],
        external_functions: ["fetch"]
      )

      {:ok, {:function_call, call, snapshot, _output}} =
        ExMonty.start(runner, %{"url" => "https://example.com"})

      call.name  # "fetch"
      call.args  # ["https://example.com"]
  """
  @spec start(runner(), map(), keyword()) :: {:ok, progress()} | {:error, error_reason()}
  def start(runner, inputs \\ %{}, opts \\ []) do
    limits = Keyword.get(opts, :limits, nil)
    input_list = Enum.map(inputs, fn {k, v} -> {to_string(k), v} end)

    case Native.start(runner, input_list, limits) do
      {:error, reason} -> {:error, reason}
      {:ok, progress} -> {:ok, progress}
      progress when is_tuple(progress) -> {:ok, progress}
    end
  rescue
    e in ErlangError ->
      {:error, e.original}
  end

  @doc """
  Resumes interactive execution from a snapshot with a result value.

  The result should be `{:ok, value}` for successful returns or
  `{:error, type, message}` for errors.

  ## Examples

      {:ok, next_progress} = ExMonty.resume(snapshot, {:ok, "response body"})
      {:ok, next_progress} = ExMonty.resume(snapshot, {:error, :runtime_error, "fetch failed"})
  """
  @spec resume(snapshot(), {:ok, term()} | {:error, atom(), String.t()}) ::
          {:ok, progress()} | {:error, error_reason()}
  def resume(snapshot, result) do
    case Native.resume(snapshot, result) do
      {:error, reason} -> {:error, reason}
      {:ok, progress} -> {:ok, progress}
      progress when is_tuple(progress) -> {:ok, progress}
    end
  rescue
    e in ErlangError ->
      {:error, e.original}
  end

  @doc """
  Resumes interactive execution from a future snapshot with results for pending calls.

  Each result is a `{call_id, {:ok, value}}` or `{call_id, {:error, type, message}}` tuple.

  ## Examples

      ids = ExMonty.pending_call_ids(futures)
      results = Enum.map(ids, fn id -> {id, {:ok, compute(id)}} end)
      {:ok, next_progress} = ExMonty.resume_futures(futures, results)
  """
  @spec resume_futures(future_snapshot(), [{non_neg_integer(), term()}]) ::
          {:ok, progress()} | {:error, error_reason()}
  def resume_futures(futures, results) do
    case Native.resume_futures(futures, results) do
      {:error, reason} -> {:error, reason}
      {:ok, progress} -> {:ok, progress}
      progress when is_tuple(progress) -> {:ok, progress}
    end
  rescue
    e in ErlangError ->
      {:error, e.original}
  end

  @doc """
  Returns the list of pending call IDs from a future snapshot.

  ## Examples

      ids = ExMonty.pending_call_ids(futures)
      # [1, 2, 3]
  """
  @spec pending_call_ids(future_snapshot()) :: [non_neg_integer()]
  def pending_call_ids(futures) do
    Native.pending_call_ids(futures)
  end

  @doc """
  Serializes a runner to a binary for storage or transfer.

  ## Examples

      {:ok, runner} = ExMonty.compile("result = x + 1", inputs: ["x"])
      {:ok, binary} = ExMonty.dump(runner)
      {:ok, restored} = ExMonty.load_runner(binary)
  """
  @spec dump(runner()) :: {:ok, binary()} | {:error, term()}
  def dump(runner) do
    {:ok, Native.dump_runner(runner)}
  rescue
    e in ErlangError ->
      {:error, e.original}
  end

  @doc """
  Deserializes a runner from a binary.

  ## Examples

      {:ok, runner} = ExMonty.load_runner(binary)
  """
  @spec load_runner(binary()) :: {:ok, runner()} | {:error, term()}
  def load_runner(binary) do
    {:ok, Native.load_runner(binary)}
  rescue
    e in ErlangError ->
      {:error, e.original}
  end

  @doc """
  Serializes a snapshot to a binary for storage or transfer.

  Note: This consumes the snapshot — it cannot be used for resumption after dumping.
  """
  @spec dump_snapshot(snapshot()) :: {:ok, binary()} | {:error, term()}
  def dump_snapshot(snapshot) do
    {:ok, Native.dump_snapshot(snapshot)}
  rescue
    e in ErlangError ->
      {:error, e.original}
  end

  @doc """
  Deserializes a snapshot from a binary.
  """
  @spec load_snapshot(binary()) :: {:ok, snapshot()} | {:error, term()}
  def load_snapshot(binary) do
    {:ok, Native.load_snapshot(binary)}
  rescue
    e in ErlangError ->
      {:error, e.original}
  end

  @doc """
  Serializes a future snapshot to a binary.

  Note: This consumes the future snapshot.
  """
  @spec dump_future_snapshot(future_snapshot()) :: {:ok, binary()} | {:error, term()}
  def dump_future_snapshot(futures) do
    {:ok, Native.dump_future_snapshot(futures)}
  rescue
    e in ErlangError ->
      {:error, e.original}
  end

  @doc """
  Deserializes a future snapshot from a binary.
  """
  @spec load_future_snapshot(binary()) :: {:ok, future_snapshot()} | {:error, term()}
  def load_future_snapshot(binary) do
    {:ok, Native.load_future_snapshot(binary)}
  rescue
    e in ErlangError ->
      {:error, e.original}
  end

  defp validate_name_list(_label, []), do: :ok

  defp validate_name_list(label, names) when is_list(names) do
    cond do
      Enum.any?(names, &(&1 == "")) ->
        {:error, "#{label} must not contain empty strings"}

      true ->
        duplicates =
          names
          |> Enum.frequencies()
          |> Enum.filter(fn {_name, count} -> count > 1 end)
          |> Enum.map(fn {name, _} -> name end)
          |> Enum.sort()

        if duplicates == [] do
          :ok
        else
          {:error, "duplicate #{label}: #{Enum.join(duplicates, ", ")}"}
        end
    end
  end
end
