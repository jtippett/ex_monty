defmodule ExMonty.ValueTest do
  use ExUnit.Case

  import ExMonty.Value, only: [to_json_safe: 1]

  defp eval_safe(code) do
    {:ok, value, _output} = ExMonty.eval(code)
    to_json_safe(value)
  end

  describe "passthrough" do
    test "JSON-native values are unchanged" do
      assert to_json_safe(nil) == nil
      assert to_json_safe(true) == true
      assert to_json_safe(1.5) == 1.5
      assert to_json_safe("x") == "x"
      assert to_json_safe(2 ** 80) == 2 ** 80

      assert eval_safe(~s|{"a": 1, "b": [1.5, None, True, "x"]}|) ==
               %{"a" => 1, "b" => [1.5, nil, true, "x"]}
    end
  end

  describe "collections" do
    test "sets become sorted lists" do
      assert eval_safe("{3, 1, 2}") == [1, 2, 3]
      assert eval_safe("frozenset([2, 1])") == [1, 2]
    end

    test "tuples become lists, recursively" do
      assert eval_safe("(1, 2)") == [1, 2]
      assert eval_safe(~s|{"a": (1, {3, 2})}|) == %{"a" => [1, [2, 3]]}
    end
  end

  describe "non-string dict keys" do
    test "keys are stringified the way json.dumps would" do
      assert eval_safe(~s|{1: "a"}|) == %{"1" => "a"}
      assert eval_safe(~s|{None: "a"}|) == %{"null" => "a"}
      assert eval_safe(~s|{True: "a"}|) == %{"true" => "a"}
      assert eval_safe(~s|{False: "a"}|) == %{"false" => "a"}
    end

    test "composite keys are stringified from their normalized form" do
      assert eval_safe(~s|{(1, 2): "a"}|) == %{"[1, 2]" => "a"}
    end
  end

  describe "non-finite floats" do
    test "become their conventional string names" do
      assert eval_safe(~s|float("nan")|) == "NaN"
      assert eval_safe(~s|float("inf")|) == "Infinity"
      assert eval_safe(~s|float("-inf")|) == "-Infinity"
    end
  end

  describe "tagged scalar values" do
    test "bytes become a string when valid UTF-8, else Base64" do
      assert to_json_safe({:bytes, "abc"}) == "abc"
      assert to_json_safe({:bytes, <<255, 254>>}) == Base.encode64(<<255, 254>>)
    end

    test "paths and reprs become their string" do
      assert to_json_safe({:path, "/tmp/x"}) == "/tmp/x"
      assert eval_safe("range(3)") == "range(0, 3)"
    end

    test "ellipsis becomes its repr" do
      assert eval_safe("...") == "Ellipsis"
    end
  end

  describe "datetime family" do
    test "dates and datetimes become ISO 8601 strings" do
      assert eval_safe("from datetime import date\ndate(2026, 5, 1)") == "2026-05-01"

      assert eval_safe("from datetime import datetime\ndatetime(2026, 5, 1, 12, 30, 45, 123456)") ==
               "2026-05-01T12:30:45.123456"

      assert eval_safe(
               "from datetime import datetime, timezone\ndatetime(2026, 5, 1, 12, 0, 0, tzinfo=timezone.utc)"
             ) == "2026-05-01T12:00:00+00:00"
    end

    test "timedeltas and timezones become plain maps" do
      assert eval_safe("from datetime import timedelta\ntimedelta(days=2, seconds=10)") ==
               %{"days" => 2, "seconds" => 10, "microseconds" => 0}

      assert to_json_safe({:timezone, %{offset_seconds: 3600, name: nil}}) ==
               %{"offset_seconds" => 3600, "name" => nil}
    end
  end

  describe "structured values" do
    test "dataclasses become their field map" do
      dataclass = %ExMonty.Dataclass{name: "Point", fields: %{"x" => 1, "y" => {2, 3}}}
      assert to_json_safe(dataclass) == %{"x" => 1, "y" => [2, 3]}
    end

    test "named tuples become their field map" do
      assert to_json_safe({:named_tuple, "Point", [{"x", 1}, {"y", {2, 3}}]}) ==
               %{"x" => 1, "y" => [2, 3]}
    end

    test "file handles become their field map" do
      assert to_json_safe({:file_handle, %{path: "/tmp/x", mode: "r", position: 0}}) ==
               %{"path" => "/tmp/x", "mode" => "r", "position" => 0}
    end
  end
end
