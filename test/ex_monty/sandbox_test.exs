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
end
