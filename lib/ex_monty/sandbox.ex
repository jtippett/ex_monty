defmodule ExMonty.Sandbox do
  @moduledoc """
  High-level handler for interactive Python execution.

  The Sandbox automates the start/resume loop by dispatching name lookups,
  function calls, dataclass method calls, and OS calls to handler callbacks.

  External function names are auto-detected at runtime: when Python code
  references an undefined name, the sandbox checks the `:functions` map and
  `:handler` module to decide whether to provide a function object or raise
  `NameError`. No upfront declaration of external functions is needed.

  Dataclass method calls are dispatched through the same function handlers as
  regular external function calls. The method name is looked up in the `:functions`
  map or dispatched to `handle_function/3` in the `:handler` module.

  ## Module-based Handler

      defmodule MyHandler do
        @behaviour ExMonty.Sandbox

        @impl true
        def handle_function("fetch", [url], _kwargs) do
          case Req.get(url) do
            {:ok, resp} -> {:ok, resp.body}
            {:error, _} -> {:error, :runtime_error, "fetch failed"}
          end
        end

        @impl true
        def handle_os(:read_text, [{:path, path}], _kwargs) do
          case File.read(path) do
            {:ok, content} -> {:ok, content}
            {:error, reason} -> {:error, :file_not_found_error, to_string(reason)}
          end
        end
      end

      {:ok, result, output} = ExMonty.Sandbox.run(code,
        inputs: %{"url" => "https://example.com"},
        handler: MyHandler,
        limits: %{max_duration_secs: 5.0}
      )

  ## Function Map Handler

      {:ok, result, output} = ExMonty.Sandbox.run(code,
        inputs: %{"x" => 1},
        functions: %{
          "fetch" => fn [url], _kwargs -> {:ok, "response"} end
        }
      )

  ## Clock Handlers

  `date.today()` and `datetime.now()` are surfaced as `:date_today` and
  `:datetime_now` os calls. Provide handlers in the `:os` map to control what
  "now" means (deterministic tests, time-travel, request timestamps, etc.):

      now = DateTime.utc_now()

      {:ok, _result, _} = ExMonty.Sandbox.run(
        "from datetime import datetime, timezone\\ndatetime.now(tz=timezone.utc).year",
        os: %{
          datetime_now: fn _args, _kwargs ->
            {:ok, {:datetime, %{
              year: now.year, month: now.month, day: now.day,
              hour: now.hour, minute: now.minute, second: now.second,
              microsecond: elem(now.microsecond, 0),
              offset_seconds: 0, tz_name: nil
            }}}
          end
        }
      )

  ## Pseudo Filesystem

  Pass an `ExMonty.PseudoFS` as the `:os` option for sandboxed filesystem access:

      fs = ExMonty.PseudoFS.new()
        |> ExMonty.PseudoFS.put_file("/data/input.txt", "hello world")

      {:ok, result, output} = ExMonty.Sandbox.run(
        "from pathlib import Path; Path('/data/input.txt').read_text()",
        os: fs
      )
  """

  @type handler_result :: {:ok, term()} | {:error, atom(), String.t()}

  @doc """
  Called when Python code invokes an external function.

  Should return `{:ok, value}` on success or `{:error, exc_type, message}` on failure.
  """
  @callback handle_function(name :: String.t(), args :: list(), kwargs :: map()) ::
              handler_result()

  @doc """
  Called when Python code performs an OS/filesystem operation.

  Optional — defaults to returning an error for all OS calls.
  """
  @callback handle_os(function :: atom(), args :: list(), kwargs :: map()) :: handler_result()

  @doc """
  Called when Python code references an undefined name.

  Return `{:ok, value}` to provide the value, or `:undefined` to raise `NameError`.
  For external functions, return `{:ok, {:function, name}}`.
  """
  @callback handle_name_lookup(name :: String.t()) :: {:ok, term()} | :undefined

  @optional_callbacks [handle_os: 3, handle_name_lookup: 1]

  @doc """
  Compiles and runs Python code with automatic handler dispatch.

  ## Options

    * `:inputs` - map of input variable names to values (default: `%{}`)
    * `:handler` - module implementing `ExMonty.Sandbox` behaviour
    * `:functions` - map of function name strings to handler fns `(args, kwargs -> result)`
    * `:os` - OS call handler. Can be:
      * An `ExMonty.PseudoFS` struct for in-memory filesystem
      * A map of `%{atom => fn args, kwargs -> result}` for per-function handlers
    * `:limits` - resource limits map (default: `nil`)
    * `:script_name` - script name for tracebacks (default: `"main.py"`)

  Either `:handler` or `:functions` must be provided for external function calls.
  OS calls require either `:os` or `handle_os/3` in the `:handler` module.

  ## Examples

      {:ok, result, output} = ExMonty.Sandbox.run(
        "result = fetch('https://example.com')",
        handler: MyHandler
      )

      {:ok, result, output} = ExMonty.Sandbox.run(
        "result = double(21)",
        functions: %{
          "double" => fn [x], _kwargs -> {:ok, x * 2} end
        }
      )

      # With pseudo filesystem
      fs = ExMonty.PseudoFS.new()
        |> ExMonty.PseudoFS.put_file("/config.json", ~s({"key": "value"}))

      {:ok, result, output} = ExMonty.Sandbox.run(
        ~s(from pathlib import Path; Path('/config.json').read_text()),
        os: fs
      )
  """
  @spec run(String.t(), keyword()) :: {:ok, term(), String.t()} | {:error, term()}
  def run(code, opts \\ []) do
    inputs = Keyword.get(opts, :inputs, %{})
    handler = Keyword.get(opts, :handler)
    functions = opts |> Keyword.get(:functions, %{}) |> normalize_function_handlers()
    os_handlers = opts |> Keyword.get(:os, %{}) |> normalize_os_handlers()
    limits = Keyword.get(opts, :limits, nil)
    script_name = Keyword.get(opts, :script_name, "main.py")
    mounts = Keyword.get(opts, :mounts)

    input_names = inputs |> Map.keys() |> Enum.map(&to_string/1) |> Enum.sort()

    compile_opts = [
      inputs: input_names,
      script_name: script_name
    ]

    with {:ok, runner} <- ExMonty.compile(code, compile_opts) do
      run_with_optional_mounts(runner, inputs, limits, %{
        handler: handler,
        functions: functions,
        os: os_handlers,
        mounts: mounts
      })
    end
  end

  defp run_with_optional_mounts(runner, inputs, limits, %{mounts: nil} = state) do
    with {:ok, progress} <- ExMonty.start(runner, inputs, limits: limits) do
      state = Map.put(state, :lease, nil)
      loop(progress, state, "")
    end
  end

  defp run_with_optional_mounts(runner, inputs, limits, %{mounts: %ExMonty.Mount{} = mounts} = state) do
    case ExMonty.Mount.checkout(mounts) do
      {:error, :mount_in_use} = err ->
        err

      {:ok, lease} ->
        try do
          state = Map.put(state, :lease, lease)

          with {:ok, progress} <-
                 ExMonty.start_with_mounts(runner, lease, inputs, limits: limits) do
            loop(progress, state, "")
          end
        after
          ExMonty.Mount.release(lease)
        end
    end
  end

  defp loop(progress, state, acc_output) do
    case progress do
      {:name_lookup, name, snapshot, output} ->
        acc_output = acc_output <> output
        result = resolve_name(name, state)

        case resume_from(snapshot, result, state) do
          {:ok, next_progress} ->
            loop(next_progress, state, acc_output)

          {:error, reason} ->
            {:error, reason}
        end

      {:function_call, %ExMonty.FunctionCall{} = call, snapshot, output} ->
        acc_output = acc_output <> output
        result = dispatch_function(call.name, call.args, call.kwargs, state)

        case resume_from(snapshot, result, state) do
          {:ok, next_progress} ->
            loop(next_progress, state, acc_output)

          {:error, reason} ->
            {:error, reason}
        end

      {:method_call, %ExMonty.FunctionCall{} = call, snapshot, output} ->
        acc_output = acc_output <> output
        result = dispatch_function(call.name, call.args, call.kwargs, state)

        case resume_from(snapshot, result, state) do
          {:ok, next_progress} ->
            loop(next_progress, state, acc_output)

          {:error, reason} ->
            {:error, reason}
        end

      {:os_call, %ExMonty.OsCall{} = call, snapshot, output} ->
        acc_output = acc_output <> output
        {state, result} = dispatch_os(call.function, call.args, call.kwargs, state)

        case resume_from(snapshot, result, state) do
          {:ok, next_progress} ->
            loop(next_progress, state, acc_output)

          {:error, reason} ->
            {:error, reason}
        end

      {:resolve_futures, futures, output} ->
        acc_output = acc_output <> output
        ids = ExMonty.pending_call_ids(futures)

        results =
          Enum.map(ids, fn id ->
            {id, {:ok, nil}}
          end)

        case ExMonty.resume_futures(futures, results) do
          {:ok, next_progress} ->
            loop(next_progress, state, acc_output)

          {:error, reason} ->
            {:error, reason}
        end

      {:complete, value, output} ->
        {:ok, value, acc_output <> output}
    end
  end

  defp resolve_name(name, state) do
    cond do
      Map.has_key?(state.functions, name) ->
        {:ok, {:function, name}}

      state.handler != nil and function_exported?(state.handler, :handle_name_lookup, 1) ->
        try do
          state.handler.handle_name_lookup(name)
        rescue
          e -> {:error, :runtime_error, Exception.message(e)}
        end
        |> case do
          {:ok, _} = ok -> ok
          :undefined -> :undefined
          {:error, _, _} = err -> err
          _ -> :undefined
        end

      state.handler != nil and function_exported?(state.handler, :handle_function, 3) ->
        # If the handler has handle_function, assume it can handle any name
        {:ok, {:function, name}}

      true ->
        :undefined
    end
  end

  defp dispatch_function(name, args, kwargs, state) do
    cond do
      Map.has_key?(state.functions, name) ->
        try do
          state.functions[name].(args, kwargs)
        rescue
          e -> {:error, :runtime_error, Exception.message(e)}
        end
        |> normalize_handler_result()

      state.handler != nil and function_exported?(state.handler, :handle_function, 3) ->
        try do
          state.handler.handle_function(name, args, kwargs)
        rescue
          e -> {:error, :runtime_error, Exception.message(e)}
        end
        |> normalize_handler_result()

      true ->
        {:error, :name_error, "function '#{name}' is not defined"}
    end
  end

  defp dispatch_os(function, args, kwargs, state) do
    os = state.os

    {new_os, result} =
      cond do
        is_struct(os, ExMonty.PseudoFS) ->
          {new_fs, result} =
            try do
              case ExMonty.PseudoFS.handle_os(os, function, args, kwargs) do
                {%ExMonty.PseudoFS{} = new_fs, result} -> {new_fs, result}
                result -> {os, result}
              end
            rescue
              e -> {os, {:error, :runtime_error, Exception.message(e)}}
            end

          {new_fs, normalize_handler_result(result)}

        is_map(os) and Map.has_key?(os, function) ->
          result =
            try do
              os[function].(args, kwargs)
            rescue
              e -> {:error, :runtime_error, Exception.message(e)}
            end

          {os, normalize_handler_result(result)}

        state.handler != nil and function_exported?(state.handler, :handle_os, 3) ->
          result =
            try do
              state.handler.handle_os(function, args, kwargs)
            rescue
              e -> {:error, :runtime_error, Exception.message(e)}
            end

          {os, normalize_handler_result(result)}

        # When a mount lease is active, defer to upstream's
        # OsFunction::on_no_handler (PermissionError on FS, RuntimeError
        # on non-FS) by signalling :no_handler back to Rust.
        state.lease != nil ->
          {os, :no_handler}

        true ->
          {os, {:error, :os_error, "OS operation '#{function}' is not permitted"}}
      end

    {%{state | os: new_os}, result}
  end

  # Routes resume calls through the mount-aware variants when a lease is
  # active so the Rust loop intercepts mount-handled FS ops.
  defp resume_from(snapshot, result, %{lease: nil}), do: ExMonty.resume(snapshot, result)

  defp resume_from(snapshot, result, %{lease: lease}),
    do: ExMonty.resume_with_mounts(snapshot, result, lease)

  defp normalize_handler_result({:ok, _} = ok), do: ok

  defp normalize_handler_result({:error, type, message}) when is_atom(type) do
    {:error, type, to_string(message)}
  end

  defp normalize_handler_result({:error, message}) do
    {:error, :runtime_error, to_string(message)}
  end

  defp normalize_handler_result(other) do
    {:error, :runtime_error, "handler returned invalid result: #{inspect(other)}"}
  end

  defp normalize_function_handlers(functions) when is_map(functions) do
    Enum.into(functions, %{}, fn {k, v} -> {to_string(k), v} end)
  end

  defp normalize_function_handlers(functions), do: functions

  @os_function_by_name %{
    "exists" => :exists,
    "is_file" => :is_file,
    "is_dir" => :is_dir,
    "is_symlink" => :is_symlink,
    "read_text" => :read_text,
    "read_bytes" => :read_bytes,
    "write_text" => :write_text,
    "write_bytes" => :write_bytes,
    "mkdir" => :mkdir,
    "unlink" => :unlink,
    "rmdir" => :rmdir,
    "iterdir" => :iterdir,
    "stat" => :stat,
    "rename" => :rename,
    "resolve" => :resolve,
    "absolute" => :absolute,
    "getenv" => :getenv,
    "get_environ" => :get_environ,
    "date_today" => :date_today,
    "datetime_now" => :datetime_now
  }

  defp normalize_os_handlers(%ExMonty.PseudoFS{} = fs), do: fs

  defp normalize_os_handlers(os) when is_map(os) do
    Enum.reduce(os, %{}, fn {k, v}, acc ->
      case os_key_to_atom(k) do
        {:ok, atom} -> Map.put(acc, atom, v)
        :error -> acc
      end
    end)
  end

  defp normalize_os_handlers(os), do: os

  defp os_key_to_atom(key) when is_atom(key) do
    if key in Map.values(@os_function_by_name), do: {:ok, key}, else: :error
  end

  defp os_key_to_atom(key) when is_binary(key) do
    case Map.fetch(@os_function_by_name, key) do
      {:ok, atom} -> {:ok, atom}
      :error -> :error
    end
  end

  defp os_key_to_atom(_), do: :error
end
