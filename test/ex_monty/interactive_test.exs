defmodule ExMonty.InteractiveTest do
  use ExUnit.Case

  describe "start/resume" do
    test "single function call" do
      {:ok, runner} =
        ExMonty.compile("result = fetch(url)\nresult",
          inputs: ["url"],
          external_functions: ["fetch"]
        )

      {:ok, progress} = ExMonty.start(runner, %{"url" => "https://example.com"})

      assert {:function_call, call, snapshot, _output} = progress
      assert %ExMonty.FunctionCall{} = call
      assert call.name == "fetch"
      assert call.args == ["https://example.com"]

      {:ok, next} = ExMonty.resume(snapshot, {:ok, "response body"})
      assert {:complete, "response body", _output} = next
    end

    test "multiple sequential function calls" do
      code = """
      a = fetch('url1')
      b = fetch('url2')
      a + ' ' + b
      """

      {:ok, runner} = ExMonty.compile(code, external_functions: ["fetch"])
      {:ok, progress} = ExMonty.start(runner)

      # First call
      assert {:function_call, call1, snap1, _} = progress
      assert call1.name == "fetch"
      assert call1.args == ["url1"]

      {:ok, progress2} = ExMonty.resume(snap1, {:ok, "hello"})

      # Second call
      assert {:function_call, call2, snap2, _} = progress2
      assert call2.name == "fetch"
      assert call2.args == ["url2"]

      {:ok, final} = ExMonty.resume(snap2, {:ok, "world"})
      assert {:complete, "hello world", _} = final
    end

    test "function call with error response" do
      code = """
      try:
          result = fetch('bad_url')
      except RuntimeError as e:
          result = str(e)
      result
      """

      {:ok, runner} = ExMonty.compile(code, external_functions: ["fetch"])
      {:ok, progress} = ExMonty.start(runner)

      assert {:function_call, _call, snapshot, _} = progress

      {:ok, final} = ExMonty.resume(snapshot, {:error, :runtime_error, "connection failed"})
      assert {:complete, "connection failed", _} = final
    end

    test "function call with kwargs" do
      code = """
      result = fetch('url', timeout=30)
      result
      """

      {:ok, runner} = ExMonty.compile(code, external_functions: ["fetch"])
      {:ok, progress} = ExMonty.start(runner)

      assert {:function_call, call, _snapshot, _} = progress
      assert call.name == "fetch"
      assert call.args == ["url"]
      assert is_map(call.kwargs)
    end

    test "no external functions - runs to completion" do
      {:ok, runner} = ExMonty.compile("2 + 2")
      {:ok, progress} = ExMonty.start(runner)
      assert {:complete, 4, ""} = progress
    end

    test "snapshot is consumed after resume" do
      {:ok, runner} =
        ExMonty.compile("fetch('url')", external_functions: ["fetch"])

      {:ok, {:function_call, _call, snapshot, _}} = ExMonty.start(runner)

      # First resume succeeds
      {:ok, _} = ExMonty.resume(snapshot, {:ok, "result"})

      # Second resume should fail - snapshot consumed
      assert {:error, _} = ExMonty.resume(snapshot, {:ok, "result2"})
    end

    test "start with resource limits" do
      {:ok, runner} = ExMonty.compile("2 + 2")
      {:ok, progress} = ExMonty.start(runner, %{}, limits: %{max_duration_secs: 5.0})
      assert {:complete, 4, _} = progress
    end
  end
end
