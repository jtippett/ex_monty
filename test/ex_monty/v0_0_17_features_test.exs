defmodule ExMonty.V0017FeaturesTest do
  @moduledoc """
  Smoke tests for Python features added between monty v0.0.8 and v0.0.17.

  These features come from upstream and don't change the ExMonty API — but
  they affect what Python code users can run, so we lock in the surface.
  """
  use ExUnit.Case

  describe "json module" do
    test "loads and dumps round-trip" do
      code = """
      import json
      data = json.loads('{"name": "alice", "age": 30}')
      json.dumps(data)
      """

      assert {:ok, ~s({"name": "alice", "age": 30}), ""} = ExMonty.eval(code)
    end

    test "loads decodes nested structures to native types" do
      code = """
      import json
      d = json.loads('{"items": [1, 2, 3], "nested": {"k": true}}')
      (d["items"], d["nested"]["k"])
      """

      assert {:ok, {[1, 2, 3], true}, ""} = ExMonty.eval(code)
    end

    test "dumps handles None, True, False" do
      assert {:ok, "[null, true, false]", ""} =
               ExMonty.eval("import json\njson.dumps([None, True, False])")
    end
  end

  describe "multi-module import" do
    test "import a, b, c" do
      assert {:ok, {3.141592653589793, "[1, 2, 3]"}, ""} =
               ExMonty.eval("import math, json\n(math.pi, json.dumps([1,2,3]))")
    end
  end

  describe "chain assignment" do
    test "a = b = c = value" do
      assert {:ok, {7, 7, 7}, ""} = ExMonty.eval("a = b = c = 7\n(a, b, c)")
    end
  end

  describe "nested subscript assignment" do
    test "d[k][i] = v" do
      code = """
      d = {"k": [1, 2, 3]}
      d["k"][1] = 99
      d
      """

      assert {:ok, %{"k" => [1, 99, 3]}, ""} = ExMonty.eval(code)
    end
  end

  describe "zip strict mode" do
    test "strict=False (default) truncates to shorter" do
      assert {:ok, [{1, 4}, {2, 5}], ""} =
               ExMonty.eval("list(zip([1,2,3], [4,5], strict=False))")
    end

    test "strict=True raises ValueError on length mismatch" do
      assert {:error, %ExMonty.Exception{type: :value_error}} =
               ExMonty.eval("list(zip([1,2,3], [4,5], strict=True))")
    end
  end

  describe "str.expandtabs" do
    test "default tabsize" do
      assert {:ok, "a       b", ""} = ExMonty.eval("\"a\\tb\".expandtabs()")
    end

    test "explicit tabsize" do
      assert {:ok, "a   b   c", ""} = ExMonty.eval("\"a\\tb\\tc\".expandtabs(4)")
    end
  end

  describe "hasattr / setattr on host-provided objects" do
    test "hasattr on dataclass passed from Elixir" do
      functions = %{
        "make_user" => fn _args, _kwargs ->
          {:ok,
           %ExMonty.Dataclass{name: "User", fields: %{"name" => "alice"}, frozen: true}}
        end
      }

      code = """
      u = make_user()
      (hasattr(u, "name"), hasattr(u, "missing"))
      """

      assert {:ok, {true, false}, ""} =
               ExMonty.Sandbox.run(code, functions: functions)
    end

    test "setattr on mutable dataclass" do
      functions = %{
        "make_user" => fn _args, _kwargs ->
          {:ok,
           %ExMonty.Dataclass{name: "User", fields: %{"name" => "alice"}, frozen: false}}
        end
      }

      code = """
      u = make_user()
      setattr(u, "name", "bob")
      u.name
      """

      assert {:ok, "bob", ""} = ExMonty.Sandbox.run(code, functions: functions)
    end
  end

  describe "INT_MAX_STR_DIGITS guard" do
    test "rejects oversized integer string conversion" do
      code = "int(\"1\" * 5000)"

      assert {:error, %ExMonty.Exception{type: :value_error, message: msg}} =
               ExMonty.eval(code)

      assert msg =~ "Exceeds the limit"
    end

    test "accepts strings under the limit" do
      assert {:ok, _bigint, ""} = ExMonty.eval("int(\"1\" * 100)")
    end
  end
end
