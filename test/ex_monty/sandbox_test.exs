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
        ExMonty.Sandbox.run("double(21)",
          handler: TestHandler,
          external_functions: ["double"]
        )

      assert result == 42
    end

    test "module handler with string return" do
      {:ok, result, _output} =
        ExMonty.Sandbox.run("greet('World')",
          handler: TestHandler,
          external_functions: ["greet"]
        )

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
