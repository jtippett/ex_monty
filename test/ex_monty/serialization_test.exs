defmodule ExMonty.SerializationTest do
  use ExUnit.Case

  describe "runner dump/load" do
    test "roundtrip runner" do
      {:ok, runner} = ExMonty.compile("x + 1", inputs: ["x"])
      {:ok, binary} = ExMonty.dump(runner)
      assert is_binary(binary)

      {:ok, restored} = ExMonty.load_runner(binary)
      assert {:ok, 2, ""} = ExMonty.run(restored, %{"x" => 1})
    end

    test "restored runner is reusable" do
      {:ok, runner} = ExMonty.compile("x * 2", inputs: ["x"])
      {:ok, binary} = ExMonty.dump(runner)
      {:ok, restored} = ExMonty.load_runner(binary)

      assert {:ok, 10, ""} = ExMonty.run(restored, %{"x" => 5})
      assert {:ok, 20, ""} = ExMonty.run(restored, %{"x" => 10})
    end

    test "original runner still works after dump" do
      {:ok, runner} = ExMonty.compile("x + 1", inputs: ["x"])
      {:ok, _binary} = ExMonty.dump(runner)
      assert {:ok, 2, ""} = ExMonty.run(runner, %{"x" => 1})
    end
  end

  describe "snapshot dump/load" do
    test "roundtrip snapshot" do
      {:ok, runner} = ExMonty.compile("result = fetch('url')\nresult")

      {:ok, {:function_call, _call, snapshot, _}} = ExMonty.start(runner)

      # Dump the snapshot
      {:ok, binary} = ExMonty.dump_snapshot(snapshot)
      assert is_binary(binary)

      # Load and resume
      {:ok, restored} = ExMonty.load_snapshot(binary)
      {:ok, final} = ExMonty.resume(restored, {:ok, "response"})
      assert {:complete, "response", _} = final
    end

    test "roundtrip preserves cumulative output limits" do
      code = """
      print("x" * 200_000)
      external()
      print("y" * 200_000)
      """

      {:ok, runner} = ExMonty.compile(code)

      assert {:ok, {:function_call, _call, snapshot, output}} =
               ExMonty.start(runner, %{}, limits: %{max_memory: 300_000})

      assert byte_size(output) > 200_000
      assert {:ok, binary} = ExMonty.dump_snapshot(snapshot)
      assert {:ok, restored} = ExMonty.load_snapshot(binary)
      assert {:error, _} = ExMonty.resume(restored, {:ok, nil})
    end

    test "corrupt or unversioned snapshot data is rejected cleanly" do
      assert {:error, _} = ExMonty.load_snapshot(<<>>)
      assert {:error, _} = ExMonty.load_snapshot(<<0, 1, 2, 3>>)
      assert {:error, _} = ExMonty.load_future_snapshot(<<0, 1, 2, 3>>)
    end

    test "a valid snapshot with appended trailing bytes is rejected" do
      {:ok, runner} = ExMonty.compile("external()")

      assert {:ok, {:function_call, _call, snapshot, _output}} = ExMonty.start(runner)
      assert {:ok, binary} = ExMonty.dump_snapshot(snapshot)

      assert {:error, _} = ExMonty.load_snapshot(binary <> <<0>>)
      assert {:error, _} = ExMonty.load_snapshot(binary <> "garbage")
    end
  end
end
