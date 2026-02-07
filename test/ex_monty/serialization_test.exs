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
      {:ok, runner} =
        ExMonty.compile("result = fetch('url')\nresult",
          external_functions: ["fetch"]
        )

      {:ok, {:function_call, _call, snapshot, _}} = ExMonty.start(runner)

      # Dump the snapshot
      {:ok, binary} = ExMonty.dump_snapshot(snapshot)
      assert is_binary(binary)

      # Load and resume
      {:ok, restored} = ExMonty.load_snapshot(binary)
      {:ok, final} = ExMonty.resume(restored, {:ok, "response"})
      assert {:complete, "response", _} = final
    end
  end
end
