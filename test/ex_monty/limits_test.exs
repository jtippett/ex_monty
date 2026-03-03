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

    test "catchable RecursionError" do
      code = """
      def recurse(n):
          return recurse(n + 1)

      try:
          recurse(0)
      except RecursionError:
          result = 'caught'
      result
      """

      {:ok, runner} = ExMonty.compile(code)
      assert {:ok, "caught", ""} = ExMonty.run(runner, %{}, limits: %{max_recursion_depth: 50})
    end

    test "combined limits with normal code succeeds" do
      {:ok, runner} = ExMonty.compile("2 + 2")

      assert {:ok, 4, ""} =
               ExMonty.run(runner, %{},
                 limits: %{
                   max_duration_secs: 5.0,
                   max_recursion_depth: 100,
                   max_allocations: 10000,
                   max_memory: 100_000
                 }
               )
    end

    test "gc_interval smoke test" do
      code = """
      x = []
      for i in range(100):
          x.append(i)
      len(x)
      """

      {:ok, runner} = ExMonty.compile(code)
      assert {:ok, 100, ""} = ExMonty.run(runner, %{}, limits: %{gc_interval: 50})
    end
  end
end
