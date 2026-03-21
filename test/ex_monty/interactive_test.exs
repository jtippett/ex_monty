defmodule ExMonty.InteractiveTest do
  use ExUnit.Case

  describe "start/resume" do
    test "single function call" do
      {:ok, runner} =
        ExMonty.compile("result = fetch(url)\nresult", inputs: ["url"])

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

      {:ok, runner} = ExMonty.compile(code)
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

      {:ok, runner} = ExMonty.compile(code)
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

      {:ok, runner} = ExMonty.compile(code)
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
      {:ok, runner} = ExMonty.compile("fetch('url')")

      {:ok, {:function_call, _call, snapshot, _}} = ExMonty.start(runner)

      # First resume succeeds
      {:ok, _} = ExMonty.resume(snapshot, {:ok, "result"})

      # Second resume should fail - snapshot consumed
      assert {:error, _} = ExMonty.resume(snapshot, {:ok, "result2"})
    end

    test "method_call progress tag for dataclass methods" do
      code = """
      o = make_obj()
      o.do_thing()
      """

      {:ok, runner} = ExMonty.compile(code)
      {:ok, progress} = ExMonty.start(runner)

      # First call: make_obj returns a dataclass with methods
      assert {:function_call, call1, snap1, _} = progress
      assert call1.name == "make_obj"

      dc = %ExMonty.Dataclass{
        name: "Obj",
        fields: %{"x" => 1},
        frozen: false
      }

      {:ok, progress2} = ExMonty.resume(snap1, {:ok, dc})

      # Second call should be a method_call tag
      assert {:method_call, call2, snap2, _} = progress2
      assert call2.name == "do_thing"

      {:ok, final} = ExMonty.resume(snap2, {:ok, "done"})
      assert {:complete, "done", _} = final
    end

    test "start with resource limits" do
      {:ok, runner} = ExMonty.compile("2 + 2")
      {:ok, progress} = ExMonty.start(runner, %{}, limits: %{max_duration_secs: 5.0})
      assert {:complete, 4, _} = progress
    end
  end

  describe "name_lookup" do
    test "name lookup for value reference" do
      # Referencing a name without calling it triggers NameLookup
      code = """
      x = config_value
      x + 1
      """

      {:ok, runner} = ExMonty.compile(code)
      {:ok, progress} = ExMonty.start(runner)

      assert {:name_lookup, "config_value", snapshot, _output} = progress

      {:ok, next} = ExMonty.resume(snapshot, {:ok, 42})
      assert {:complete, 43, _} = next
    end

    test "name lookup with undefined returns NameError" do
      code = "x = unknown_name"

      {:ok, runner} = ExMonty.compile(code)
      {:ok, progress} = ExMonty.start(runner)

      assert {:name_lookup, "unknown_name", snapshot, _output} = progress

      assert {:error, exc} = ExMonty.resume(snapshot, :undefined)
      assert %{type: :name_error} = exc
    end

    test "name lookup provides function object for later call" do
      # Loading a function reference, then calling it later
      code = """
      callback = my_func
      callback(10)
      """

      {:ok, runner} = ExMonty.compile(code)
      {:ok, progress} = ExMonty.start(runner)

      # First: name lookup for my_func
      assert {:name_lookup, "my_func", snapshot, _} = progress

      # Provide a function object
      {:ok, progress2} = ExMonty.resume(snapshot, {:ok, {:function, "my_func"}})

      # Then the function is called
      assert {:function_call, call, snap2, _} = progress2
      assert call.name == "my_func"
      assert call.args == [10]

      {:ok, final} = ExMonty.resume(snap2, {:ok, "result"})
      assert {:complete, "result", _} = final
    end

    test "name lookup with sandbox auto-resolves known functions" do
      {:ok, result, _output} =
        ExMonty.Sandbox.run(
          "callback = double\ncallback(21)",
          functions: %{
            "double" => fn [x], _kwargs -> {:ok, x * 2} end
          }
        )

      assert result == 42
    end
  end
end
