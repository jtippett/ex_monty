defmodule ExMonty.Sandbox do
  @moduledoc """
  High-level handler for interactive Python execution.

  The Sandbox automates the start/resume loop by dispatching function calls
  and OS calls to handler callbacks.

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
        def handle_os(:read_text, [path], _kwargs) do
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
  @callback handle_function(name :: String.t(), args :: list(), kwargs :: map()) :: handler_result()

  @doc """
  Called when Python code performs an OS/filesystem operation.

  Optional — defaults to returning an error for all OS calls.
  """
  @callback handle_os(function :: atom(), args :: list(), kwargs :: map()) :: handler_result()

  @optional_callbacks [handle_os: 3]

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
    * `:external_functions` - list of external function names (auto-detected from `:functions`)
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
    functions = Keyword.get(opts, :functions, %{})
    os_handlers = Keyword.get(opts, :os, %{})
    limits = Keyword.get(opts, :limits, nil)
    script_name = Keyword.get(opts, :script_name, "main.py")

    external_fns =
      Keyword.get_lazy(opts, :external_functions, fn ->
        Map.keys(functions)
      end)

    input_names = inputs |> Map.keys() |> Enum.map(&to_string/1)

    compile_opts = [
      inputs: input_names,
      external_functions: external_fns,
      script_name: script_name
    ]

    with {:ok, runner} <- ExMonty.compile(code, compile_opts),
         {:ok, progress} <- ExMonty.start(runner, inputs, limits: limits) do
      state = %{
        handler: handler,
        functions: functions,
        os: os_handlers
      }

      loop(progress, state, "")
    end
  end

  defp loop(progress, state, acc_output) do
    case progress do
      {:function_call, %ExMonty.FunctionCall{} = call, snapshot, output} ->
        acc_output = acc_output <> output
        result = dispatch_function(call.name, call.args, call.kwargs, state)

        case ExMonty.resume(snapshot, result) do
          {:ok, next_progress} ->
            loop(next_progress, state, acc_output)

          {:error, reason} ->
            {:error, reason}
        end

      {:os_call, %ExMonty.OsCall{} = call, snapshot, output} ->
        acc_output = acc_output <> output
        {state, result} = dispatch_os(call.function, call.args, call.kwargs, state)

        case ExMonty.resume(snapshot, result) do
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

  defp dispatch_function(name, args, kwargs, state) do
    cond do
      Map.has_key?(state.functions, name) ->
        try do
          state.functions[name].(args, kwargs)
        rescue
          e -> {:error, :runtime_error, Exception.message(e)}
        end

      state.handler != nil and function_exported?(state.handler, :handle_function, 3) ->
        try do
          state.handler.handle_function(name, args, kwargs)
        rescue
          e -> {:error, :runtime_error, Exception.message(e)}
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
          case ExMonty.PseudoFS.handle_os(os, function, args, kwargs) do
            {%ExMonty.PseudoFS{} = new_fs, result} -> {new_fs, result}
            result -> {os, result}
          end

        is_map(os) and Map.has_key?(os, function) ->
          result =
            try do
              os[function].(args, kwargs)
            rescue
              e -> {:error, :runtime_error, Exception.message(e)}
            end

          {os, result}

        state.handler != nil and function_exported?(state.handler, :handle_os, 3) ->
          result =
            try do
              state.handler.handle_os(function, args, kwargs)
            rescue
              e -> {:error, :runtime_error, Exception.message(e)}
            end

          {os, result}

        true ->
          {os, {:error, :os_error, "OS operation '#{function}' is not permitted"}}
      end

    {%{state | os: new_os}, result}
  end
end
