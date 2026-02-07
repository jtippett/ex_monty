defmodule ExMonty.LimitsTest do
  use ExUnit.Case

  describe "resource limits" do
    test "time limit" do
      code = """
      i = 0
      while True:
          i += 1
      """

      {:ok, runner} = ExMonty.compile(code)
      result = ExMonty.run(runner, %{}, limits: %{max_duration_secs: 0.1})
      assert {:error, _} = result
    end

    test "recursion limit" do
      code = """
      def infinite():
          return infinite()

      infinite()
      """

      {:ok, runner} = ExMonty.compile(code)
      result = ExMonty.run(runner, %{}, limits: %{max_recursion_depth: 50})
      assert {:error, _} = result
    end

    test "allocation limit" do
      code = """
      x = []
      for i in range(1000000):
          x.append([i] * 100)
      """

      {:ok, runner} = ExMonty.compile(code)
      result = ExMonty.run(runner, %{}, limits: %{max_allocations: 10, max_memory: 500})
      assert {:error, _} = result
    end

    test "default limits allow normal code" do
      assert {:ok, 42, ""} = ExMonty.eval("42")
    end
  end
end
