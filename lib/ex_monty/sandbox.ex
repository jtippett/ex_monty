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

  ## Host Filesystem Mounts

  For sandboxed access to real host directories, pass an `ExMonty.Mount`
  as the `:mounts` option:

      mounts =
        ExMonty.Mount.new!()
        |> ExMonty.Mount.add!("/data", "/var/lib/myapp/data", :read_only)

      ExMonty.Sandbox.run(code, mounts: mounts)

  `:mounts` composes with `:os` — mounts handle filesystem calls, the
  `:os` map handles non-filesystem fallbacks (`:getenv`,
  `:datetime_now`, etc.). See `ExMonty.Mount` for mode semantics, lease
  lifecycle, and security guarantees.

  ## Pseudo Filesystem

  Pass an `ExMonty.PseudoFS` as the `:os` option for sandboxed filesystem access:

      fs = ExMonty.PseudoFS.new()
        |> ExMonty.PseudoFS.put_file("/data/input.txt", "hello world")

      {:ok, result, output} = ExMonty.Sandbox.run(
        "from pathlib import Path; Path('/data/input.txt').read_text()",
        os: fs
      )
  """

  @default_callback_timeout 10_000
  @max_callback_error_chars 1_000

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
    * `:mounts` - an `ExMonty.Mount` for host filesystem access. Composes
      with `:os` (mounts handle FS calls; `:os` map handles non-FS fallback
      calls like `:getenv` or `:datetime_now`). Unmounted paths raise
      `PermissionError`.
    * `:limits` - resource limits map (default: `nil`)
    * `:script_name` - script name for tracebacks (default: `"main.py"`)
    * `:callback_timeout` - maximum milliseconds for each external callback
      (default: `10_000`; use `:infinity` to disable). Callbacks run in isolated,
      monitored processes, so `self()` and the process dictionary refer to the
      callback worker rather than the `Sandbox.run/2` caller.

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
    callback_timeout = Keyword.get(opts, :callback_timeout, @default_callback_timeout)

    input_names = inputs |> Map.keys() |> Enum.map(&to_string/1) |> Enum.sort()

    compile_opts = [
      inputs: input_names,
      script_name: script_name
    ]

    with :ok <- validate_callback_timeout(callback_timeout),
         {:ok, runner} <- ExMonty.compile(code, compile_opts) do
      run_with_optional_mounts(runner, inputs, limits, %{
        handler: handler,
        functions: functions,
        os: os_handlers,
        mounts: mounts,
        callback_timeout: callback_timeout
      })
    end
  end

  defp run_with_optional_mounts(runner, inputs, limits, %{mounts: nil} = state) do
    with {:ok, progress} <- ExMonty.start(runner, inputs, limits: limits) do
      state = Map.put(state, :lease, nil)
      loop(progress, state, [])
    end
  end

  defp run_with_optional_mounts(
         runner,
         inputs,
         limits,
         %{mounts: %ExMonty.Mount{} = mounts} = state
       ) do
    case ExMonty.Mount.checkout(mounts) do
      {:error, :mount_in_use} = err ->
        err

      {:ok, lease} ->
        try do
          state = Map.put(state, :lease, lease)

          with {:ok, progress} <-
                 ExMonty.start_with_mounts(runner, lease, inputs, limits: limits) do
            loop(progress, state, [])
          end
        after
          ExMonty.Mount.release(lease)
        end
    end
  end

  defp loop(progress, state, acc_output) do
    case progress do
      {:name_lookup, name, snapshot, output} ->
        acc_output = [output | acc_output]

        case resolve_name(name, state) do
          # Monty has no error variant for name lookups, so a raising, timed-out,
          # or error-returning handler cannot resume into a Python exception.
          # Surface the real reason instead of the opaque strict-decode failure
          # that passing an {:error, _, _} tuple to `resume/2` would produce.
          {:error, _type, message} ->
            {:error, message}

          result ->
            case resume_from(snapshot, result, state) do
              {:ok, next_progress} ->
                loop(next_progress, state, acc_output)

              {:error, reason} ->
                {:error, reason}
            end
        end

      {:function_call, %ExMonty.FunctionCall{} = call, snapshot, output} ->
        acc_output = [output | acc_output]
        result = dispatch_function(call.name, call.args, call.kwargs, state)

        case resume_from(snapshot, result, state) do
          {:ok, next_progress} ->
            loop(next_progress, state, acc_output)

          {:error, reason} ->
            {:error, reason}
        end

      {:method_call, %ExMonty.FunctionCall{} = call, snapshot, output} ->
        acc_output = [output | acc_output]
        result = dispatch_function(call.name, call.args, call.kwargs, state)

        case resume_from(snapshot, result, state) do
          {:ok, next_progress} ->
            loop(next_progress, state, acc_output)

          {:error, reason} ->
            {:error, reason}
        end

      {:os_call, %ExMonty.OsCall{} = call, snapshot, output} ->
        acc_output = [output | acc_output]
        {state, result} = dispatch_os(call.function, call.args, call.kwargs, state)

        case resume_from(snapshot, result, state) do
          {:ok, next_progress} ->
            loop(next_progress, state, acc_output)

          {:error, reason} ->
            {:error, reason}
        end

      {:resolve_futures, _futures, _output} ->
        {:error,
         "Sandbox.run/2 cannot resolve pending futures; use the low-level start/resume API"}

      {:complete, value, output} ->
        {:ok, value, acc_output |> Enum.reverse([output]) |> IO.iodata_to_binary()}
    end
  end

  defp resolve_name(name, state) do
    cond do
      Map.has_key?(state.functions, name) ->
        {:ok, {:function, name}}

      state.handler != nil and function_exported?(state.handler, :handle_name_lookup, 1) ->
        case invoke_callback(
               fn -> state.handler.handle_name_lookup(name) end,
               state.callback_timeout
             ) do
          {:ok, result} -> normalize_name_lookup_result(result)
          {:error, _, _} = error -> error
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
        case invoke_callback(
               fn -> state.functions[name].(args, kwargs) end,
               state.callback_timeout
             ) do
          {:ok, result} -> normalize_handler_result(result)
          {:error, _, _} = error -> error
        end

      state.handler != nil and function_exported?(state.handler, :handle_function, 3) ->
        case invoke_callback(
               fn -> state.handler.handle_function(name, args, kwargs) end,
               state.callback_timeout
             ) do
          {:ok, result} -> normalize_handler_result(result)
          {:error, _, _} = error -> error
        end

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
          result = invoke_callback(fn -> os[function].(args, kwargs) end, state.callback_timeout)

          {os, normalize_callback_result(result)}

        state.handler != nil and function_exported?(state.handler, :handle_os, 3) ->
          result =
            invoke_callback(
              fn -> state.handler.handle_os(function, args, kwargs) end,
              state.callback_timeout
            )

          {os, normalize_callback_result(result)}

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

  defp normalize_handler_result({:error, type, message})
       when is_atom(type) and is_binary(message) do
    {:error, type, message}
  end

  defp normalize_handler_result({:error, message}) when is_binary(message) do
    {:error, :runtime_error, message}
  end

  defp normalize_handler_result(_other) do
    {:error, :runtime_error, "handler returned an invalid result"}
  end

  defp normalize_name_lookup_result({:ok, _} = ok), do: ok
  defp normalize_name_lookup_result(:undefined), do: :undefined

  defp normalize_name_lookup_result({:error, type, message})
       when is_atom(type) and is_binary(message),
       do: {:error, type, message}

  defp normalize_name_lookup_result(_other),
    do: {:error, :runtime_error, "name lookup handler returned an invalid result"}

  defp normalize_callback_result({:ok, result}), do: normalize_handler_result(result)
  defp normalize_callback_result({:error, _, _} = error), do: error

  defp validate_callback_timeout(:infinity), do: :ok

  defp validate_callback_timeout(timeout) when is_integer(timeout) and timeout > 0,
    do: :ok

  defp validate_callback_timeout(_timeout),
    do: {:error, "callback_timeout must be a positive integer or :infinity"}

  # Execute foreign callback code outside the Sandbox.run/2 process. A tiny
  # watchdog monitors both processes: if the caller dies, the worker is killed;
  # if the worker exits (including an uncatchable `:kill`), the caller receives
  # a normal Python RuntimeError instead of being taken down with it.
  defp invoke_callback(fun, timeout) when is_function(fun, 0) do
    parent = self()
    token = make_ref()

    {worker, monitor} =
      spawn_monitor(fn ->
        callback_worker(parent, token, fun)
      end)

    deadline =
      case timeout do
        :infinity -> :infinity
        milliseconds -> System.monotonic_time(:millisecond) + milliseconds
      end

    await_callback(worker, monitor, token, deadline, timeout)
  end

  defp callback_worker(parent, token, fun) do
    worker = self()
    watchdog = spawn(fn -> callback_watchdog(parent, worker) end)

    receive do
      {:callback_watchdog_ready, ^watchdog} -> :ok
    end

    result =
      try do
        {:returned, fun.()}
      rescue
        exception -> {:raised, safe_exception_message(exception)}
      catch
        kind, reason -> {:caught, kind, safe_inspect(reason)}
      end

    send(parent, {:ex_monty_callback_done, token, result})
  end

  defp callback_watchdog(parent, worker) do
    parent_monitor = Process.monitor(parent)
    worker_monitor = Process.monitor(worker)
    send(worker, {:callback_watchdog_ready, self()})

    receive do
      {:DOWN, ^parent_monitor, :process, ^parent, _reason} ->
        Process.exit(worker, :kill)

      {:DOWN, ^worker_monitor, :process, ^worker, _reason} ->
        :ok
    end
  end

  defp await_callback(worker, monitor, token, :infinity, _original_timeout) do
    receive do
      {:ex_monty_callback_done, ^token, result} ->
        Process.demonitor(monitor, [:flush])
        callback_outcome(result)

      {:DOWN, ^monitor, :process, ^worker, _reason} ->
        {:error, :runtime_error, "callback process exited before returning"}
    end
  end

  defp await_callback(worker, monitor, token, deadline, original_timeout) do
    remaining = max(deadline - System.monotonic_time(:millisecond), 0)

    receive do
      {:ex_monty_callback_done, ^token, result} ->
        Process.demonitor(monitor, [:flush])
        callback_outcome(result)

      {:DOWN, ^monitor, :process, ^worker, _reason} ->
        {:error, :runtime_error, "callback process exited before returning"}
    after
      remaining ->
        Process.exit(worker, :kill)

        receive do
          {:DOWN, ^monitor, :process, ^worker, _reason} -> :ok
        end

        # If the callback raced the deadline, its completion signal precedes
        # the worker's DOWN signal. Remove that stale, now-ignored reply so it
        # cannot leak into the caller mailbox after a timeout.
        receive do
          {:ex_monty_callback_done, ^token, _result} -> :ok
        after
          0 -> :ok
        end

        {:error, :timeout_error, "callback timed out after #{original_timeout} ms"}
    end
  end

  defp callback_outcome({:returned, value}), do: {:ok, value}
  defp callback_outcome({:raised, message}), do: {:error, :runtime_error, message}

  defp callback_outcome({:caught, kind, reason}) do
    {:error, :runtime_error, "callback #{kind}: #{reason}"}
  end

  defp safe_exception_message(exception) do
    try do
      exception
      |> Exception.message()
      |> bounded_message()
    rescue
      _ -> "callback raised an exception"
    catch
      _, _ -> "callback raised an exception"
    end
  end

  defp safe_inspect(term) do
    try do
      term
      |> inspect(limit: 20, printable_limit: 200, width: 80, structs: false)
      |> bounded_message()
    rescue
      _ -> "unprintable callback reason"
    catch
      _, _ -> "unprintable callback reason"
    end
  end

  defp bounded_message(message) when is_binary(message),
    do: String.slice(message, 0, @max_callback_error_chars)

  defp bounded_message(_message), do: "invalid callback error message"

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
    "append_text" => :append_text,
    "append_bytes" => :append_bytes,
    "open" => :open,
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
