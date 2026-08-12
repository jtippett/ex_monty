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

    # monty v0.0.21 removed allocation counting; a request for it must fail
    # loudly rather than run with a limit the caller believes is enforced.
    test "max_allocations is rejected with a clear error" do
      {:ok, runner} = ExMonty.compile("1 + 1")
      result = ExMonty.run(runner, %{}, limits: %{max_allocations: 10})
      assert {:error, message} = result
      assert message =~ "max_allocations"
    end

    test "memory limit bounds a single large allocation" do
      {:ok, runner} = ExMonty.compile("'x' * 10_000_000")
      result = ExMonty.run(runner, %{}, limits: %{max_memory: 500_000})
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
