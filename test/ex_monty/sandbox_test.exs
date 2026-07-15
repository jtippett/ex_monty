defmodule ExMonty.SandboxTest do
  use ExUnit.Case

  describe "function map handler" do
    test "simple function call" do
      {:ok, result, _output} =
        ExMonty.Sandbox.run(
          "double(21)",
          functions: %{
            "double" => fn [x], _kwargs -> {:ok, x * 2} end
          }
        )

      assert result == 42
    end

    test "multiple different functions" do
      {:ok, result, _output} =
        ExMonty.Sandbox.run(
          "add(2, 3) + multiply(4, 5)",
          functions: %{
            "add" => fn [a, b], _kwargs -> {:ok, a + b} end,
            "multiply" => fn [a, b], _kwargs -> {:ok, a * b} end
          }
        )

      assert result == 25
    end

    test "function returning error" do
      code = """
      try:
          result = fetch('bad')
      except RuntimeError as e:
          result = str(e)
      result
      """

      {:ok, result, _output} =
        ExMonty.Sandbox.run(code,
          functions: %{
            "fetch" => fn _args, _kwargs -> {:error, :runtime_error, "network error"} end
          }
        )

      assert result == "network error"
    end

    test "function with string processing" do
      {:ok, result, _output} =
        ExMonty.Sandbox.run(
          "upper('hello world')",
          functions: %{
            "upper" => fn [s], _kwargs -> {:ok, String.upcase(s)} end
          }
        )

      assert result == "HELLO WORLD"
    end
  end

  describe "module handler" do
    defmodule TestHandler do
      @behaviour ExMonty.Sandbox

      @impl true
      def handle_function("double", [x], _kwargs), do: {:ok, x * 2}
      def handle_function("greet", [name], _kwargs), do: {:ok, "Hello, #{name}!"}

      def handle_function(name, _args, _kwargs),
        do: {:error, :name_error, "unknown function: #{name}"}
    end

    test "basic module handler" do
      {:ok, result, _output} =
        ExMonty.Sandbox.run("double(21)", handler: TestHandler)

      assert result == 42
    end

    test "module handler with string return" do
      {:ok, result, _output} =
        ExMonty.Sandbox.run("greet('World')", handler: TestHandler)

      assert result == "Hello, World!"
    end
  end

  describe "with inputs" do
    test "passes inputs to Python code" do
      {:ok, result, _output} =
        ExMonty.Sandbox.run("double(x)",
          inputs: %{"x" => 21},
          functions: %{
            "double" => fn [x], _kwargs -> {:ok, x * 2} end
          }
        )

      assert result == 42
    end
  end

  describe "output capture" do
    test "captures print output" do
      {:ok, _result, output} =
        ExMonty.Sandbox.run(
          "print('hello')\ndouble(1)",
          functions: %{
            "double" => fn [x], _kwargs -> {:ok, x * 2} end
          }
        )

      assert output =~ "hello"
    end

    test "preserves output order across many callback pauses" do
      code = """
      print('before')
      first()
      print('middle')
      second()
      print('after')
      """

      functions = %{
        "first" => fn _, _ -> {:ok, nil} end,
        "second" => fn _, _ -> {:ok, nil} end
      }

      assert {:ok, nil, "before\nmiddle\nafter\n"} =
               ExMonty.Sandbox.run(code, functions: functions)
    end
  end

  describe "multiple sequential calls" do
    test "three chained function calls" do
      functions = %{
        "step1" => fn _args, _kwargs -> {:ok, 10} end,
        "step2" => fn [x], _kwargs -> {:ok, x * 2} end,
        "step3" => fn [x], _kwargs -> {:ok, x + 5} end
      }

      code = """
      a = step1()
      b = step2(a)
      step3(b)
      """

      {:ok, result, _output} = ExMonty.Sandbox.run(code, functions: functions)
      assert result == 25
    end

    test "same function called multiple times with different args" do
      functions = %{
        "double" => fn [x], _kwargs -> {:ok, x * 2} end
      }

      code = """
      a = double(1)
      b = double(2)
      c = double(3)
      [a, b, c]
      """

      {:ok, result, _output} = ExMonty.Sandbox.run(code, functions: functions)
      assert result == [2, 4, 6]
    end
  end

  describe "sandbox with PseudoFS" do
    test "combines functions and os options" do
      fs =
        ExMonty.PseudoFS.new()
        |> ExMonty.PseudoFS.put_file("/data/input.txt", "hello")

      functions = %{
        "process" => fn [text], _kwargs -> {:ok, String.upcase(text)} end
      }

      code = """
      from pathlib import Path
      content = Path('/data/input.txt').read_text()
      process(content)
      """

      {:ok, result, _output} = ExMonty.Sandbox.run(code, os: fs, functions: functions)
      assert result == "HELLO"
    end

    test "Python reads file via PseudoFS and passes to external function" do
      fs =
        ExMonty.PseudoFS.new()
        |> ExMonty.PseudoFS.put_file("/data/numbers.txt", "1,2,3")

      functions = %{
        "parse_nums" => fn [text], _kwargs ->
          nums = text |> String.split(",") |> Enum.map(&String.to_integer/1)
          {:ok, nums}
        end
      }

      code = """
      from pathlib import Path
      data = Path('/data/numbers.txt').read_text()
      parse_nums(data)
      """

      {:ok, result, _output} = ExMonty.Sandbox.run(code, os: fs, functions: functions)
      assert result == [1, 2, 3]
    end
  end

  describe "method_call dispatch" do
    test "sandbox dispatches method_call through functions map" do
      functions = %{
        "make_obj" => fn _args, _kwargs ->
          {:ok,
           %ExMonty.Dataclass{
             name: "Obj",
             fields: %{"val" => 42},
             frozen: false
           }}
        end,
        "get_val" => fn [obj], _kwargs ->
          {:ok, obj.fields["val"]}
        end
      }

      {:ok, result, _output} =
        ExMonty.Sandbox.run("o = make_obj()\no.get_val()", functions: functions)

      assert result == 42
    end

    test "method_call error is caught by Python" do
      functions = %{
        "make_obj" => fn _args, _kwargs ->
          {:ok,
           %ExMonty.Dataclass{
             name: "Obj",
             fields: %{},
             frozen: false
           }}
        end,
        "fail" => fn _args, _kwargs ->
          {:error, :runtime_error, "method failed"}
        end
      }

      code = """
      o = make_obj()
      try:
          o.fail()
          result = "no error"
      except RuntimeError as e:
          result = str(e)
      result
      """

      {:ok, result, _output} = ExMonty.Sandbox.run(code, functions: functions)
      assert result == "method failed"
    end
  end

  describe "sandbox error edge cases" do
    test "handler raising an Elixir exception is rescued by sandbox" do
      functions = %{
        "crash" => fn _args, _kwargs -> raise "boom" end
      }

      code = """
      try:
          crash()
      except RuntimeError as e:
          result = str(e)
      result
      """

      {:ok, result, _output} = ExMonty.Sandbox.run(code, functions: functions)
      assert result == "boom"
    end

    test "handler throw and exit become Python errors" do
      for {name, handler} <- [
            {"thrower", fn _args, _kwargs -> throw(:boom) end},
            {"exiter", fn _args, _kwargs -> exit(:boom) end}
          ] do
        code = """
        try:
            #{name}()
        except RuntimeError as e:
            result = str(e)
        result
        """

        assert {:ok, result, ""} =
                 ExMonty.Sandbox.run(code, functions: %{name => handler})

        assert result =~ "boom"
      end
    end

    test "a handler that brutal-kills itself cannot kill the sandbox caller" do
      handler = fn _args, _kwargs -> Process.exit(self(), :kill) end

      code = """
      try:
          terminate()
      except RuntimeError as e:
          result = str(e)
      result
      """

      assert {:ok, result, ""} =
               ExMonty.Sandbox.run(code, functions: %{"terminate" => handler})

      assert result =~ "exited before returning"
      assert {:ok, 2, ""} = ExMonty.Sandbox.run("1 + 1")
    end

    test "callback timeout kills work and prevents late effects" do
      parent = self()

      handler = fn _args, _kwargs ->
        Process.sleep(100)
        send(parent, :late_callback_effect)
        {:ok, 1}
      end

      code = """
      try:
          slow()
      except TimeoutError as e:
          result = str(e)
      result
      """

      assert {:ok, "callback timed out after 10 ms", ""} =
               ExMonty.Sandbox.run(code,
                 functions: %{"slow" => handler},
                 callback_timeout: 10
               )

      refute_receive :late_callback_effect, 150
    end

    test "caller death cancels an in-flight callback worker" do
      test_process = self()

      handler = fn _args, _kwargs ->
        send(test_process, {:callback_started, self()})
        Process.sleep(5_000)
        send(test_process, :orphaned_callback_effect)
        {:ok, 1}
      end

      runner =
        spawn(fn ->
          ExMonty.Sandbox.run("slow()",
            functions: %{"slow" => handler},
            callback_timeout: :infinity
          )
        end)

      runner_monitor = Process.monitor(runner)
      assert_receive {:callback_started, worker}, 1_000
      worker_monitor = Process.monitor(worker)

      Process.exit(runner, :kill)

      assert_receive {:DOWN, ^runner_monitor, :process, ^runner, :killed}, 1_000
      assert_receive {:DOWN, ^worker_monitor, :process, ^worker, :killed}, 1_000
      refute_receive :orphaned_callback_effect, 50
    end

    test "invalid callback timeout is rejected before execution" do
      assert {:error, "callback_timeout must be a positive integer or :infinity"} =
               ExMonty.Sandbox.run("1 + 1", callback_timeout: 0)
    end

    defmodule RaisingLookupHandler do
      @behaviour ExMonty.Sandbox

      @impl true
      def handle_function(name, _args, _kwargs),
        do: {:error, :name_error, "unknown function: #{name}"}

      @impl true
      def handle_name_lookup(_name), do: raise("lookup exploded")
    end

    test "a raising name-lookup handler surfaces its real error, not a decode failure" do
      # Monty has no error variant for name lookups; the sandbox must not feed
      # the handler error into resume/2 (which would reject it as a malformed
      # name-lookup result) but surface the underlying reason instead.
      assert {:error, message} =
               ExMonty.Sandbox.run("undefined_symbol", handler: RaisingLookupHandler)

      assert message =~ "lookup exploded"
      refute message =~ "must be :undefined"
    end

    test "sandbox with resource limits" do
      functions = %{
        "get_val" => fn _args, _kwargs -> {:ok, 42} end
      }

      {:ok, result, _output} =
        ExMonty.Sandbox.run("get_val()",
          functions: functions,
          limits: %{max_duration_secs: 5.0}
        )

      assert result == 42
    end
  end
end
