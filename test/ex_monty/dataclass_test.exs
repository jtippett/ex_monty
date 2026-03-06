defmodule ExMonty.DataclassTest do
  use ExUnit.Case

  defp make_point do
    %ExMonty.Dataclass{name: "Point", fields: %{"x" => 1, "y" => 2}, frozen: true}
  end

  defp make_mutable_point do
    %ExMonty.Dataclass{name: "Point", fields: %{"x" => 1, "y" => 2}, frozen: false}
  end

  describe "dataclass struct" do
    test "frozen dataclass struct" do
      point = make_point()
      assert point.name == "Point"
      assert point.fields == %{"x" => 1, "y" => 2}
      assert point.frozen == true
    end

    test "mutable dataclass struct" do
      point = make_mutable_point()
      assert point.frozen == false
    end

    test "empty dataclass struct" do
      empty = %ExMonty.Dataclass{name: "Empty", fields: %{}, frozen: false}
      assert empty.name == "Empty"
      assert empty.fields == %{}
    end

    test "default frozen is false" do
      dc = %ExMonty.Dataclass{name: "Foo", fields: %{"a" => 1}}
      assert dc.frozen == false
    end

    test "new fields default to nil" do
      dc = %ExMonty.Dataclass{name: "Foo", fields: %{"a" => 1}}
      assert dc.type_id == nil
      assert dc.field_names == nil
    end
  end

  describe "dataclass round-trip through sandbox" do
    test "dataclass field access via dot notation (p.x)" do
      functions = %{
        "make_point" => fn _args, _kwargs ->
          {:ok, %ExMonty.Dataclass{name: "Point", fields: %{"x" => 10, "y" => 20}, frozen: true}}
        end
      }

      {:ok, result, _output} =
        ExMonty.Sandbox.run(
          "p = make_point()\np.x",
          functions: functions
        )

      assert result == 10
    end

    test "dataclass field access via dot notation - multiple fields" do
      functions = %{
        "make_point" => fn _args, _kwargs ->
          {:ok, %ExMonty.Dataclass{name: "Point", fields: %{"x" => 10, "y" => 20}, frozen: true}}
        end
      }

      {:ok, result, _output} =
        ExMonty.Sandbox.run(
          "p = make_point()\n[p.x, p.y]",
          functions: functions
        )

      assert result == [10, 20]
    end

    test "frozen dataclass rejects field assignment" do
      functions = %{
        "make_point" => fn _args, _kwargs ->
          {:ok, %ExMonty.Dataclass{name: "Point", fields: %{"x" => 1, "y" => 2}, frozen: true}}
        end
      }

      code = """
      p = make_point()
      try:
          p.x = 99
          result = "no error"
      except AttributeError:
          result = "frozen"
      result
      """

      {:ok, result, _output} = ExMonty.Sandbox.run(code, functions: functions)
      assert result == "frozen"
    end

    test "mutable dataclass allows field assignment" do
      functions = %{
        "make_point" => fn _args, _kwargs ->
          {:ok, %ExMonty.Dataclass{name: "Point", fields: %{"x" => 1, "y" => 2}, frozen: false}}
        end
      }

      {:ok, result, _output} =
        ExMonty.Sandbox.run(
          "p = make_point()\np.x = 99\np.x",
          functions: functions
        )

      assert result == 99
    end

    test "dataclass returned to handler preserves struct" do
      test_pid = self()

      functions = %{
        "make_point" => fn _args, _kwargs ->
          {:ok, %ExMonty.Dataclass{name: "Point", fields: %{"x" => 1, "y" => 2}, frozen: true}}
        end,
        "inspect_it" => fn [val], _kwargs ->
          send(test_pid, {:received, val})
          {:ok, "ok"}
        end
      }

      {:ok, _result, _output} =
        ExMonty.Sandbox.run(
          "p = make_point()\ninspect_it(p)",
          functions: functions
        )

      assert_receive {:received, %ExMonty.Dataclass{} = dc}
      assert dc.name == "Point"
      assert dc.fields["x"] == 1
      assert dc.fields["y"] == 2
      assert dc.frozen == true
    end

    test "dataclass in a list round-trips" do
      functions = %{
        "make_point" => fn _args, _kwargs ->
          {:ok, %ExMonty.Dataclass{name: "Point", fields: %{"x" => 5, "y" => 6}, frozen: false}}
        end
      }

      {:ok, result, _output} =
        ExMonty.Sandbox.run(
          "items = [make_point()]\nitems[0].x",
          functions: functions
        )

      assert result == 5
    end

    test "dataclass with complex field values" do
      functions = %{
        "make_data" => fn _args, _kwargs ->
          {:ok,
           %ExMonty.Dataclass{
             name: "Data",
             fields: %{"items" => [1, 2, 3], "label" => "test"},
             frozen: true
           }}
        end
      }

      {:ok, result, _output} =
        ExMonty.Sandbox.run(
          "d = make_data()\nd.items",
          functions: functions
        )

      assert result == [1, 2, 3]
    end

    test "factory function passes args into dataclass fields" do
      functions = %{
        "make_user" => fn [name], _kwargs ->
          {:ok,
           %ExMonty.Dataclass{
             name: "User",
             fields: %{"name" => name, "active" => true},
             frozen: true
           }}
        end
      }

      code = """
      u = make_user("Alice")
      [u.name, u.active]
      """

      {:ok, result, _output} = ExMonty.Sandbox.run(code, functions: functions)
      assert result == ["Alice", true]
    end

    test "empty dataclass round-trips" do
      test_pid = self()

      functions = %{
        "make_empty" => fn _args, _kwargs ->
          {:ok, %ExMonty.Dataclass{name: "Empty", fields: %{}, frozen: true}}
        end,
        "check" => fn [val], _kwargs ->
          send(test_pid, {:received, val})
          {:ok, "ok"}
        end
      }

      {:ok, _result, _output} =
        ExMonty.Sandbox.run("e = make_empty()\ncheck(e)", functions: functions)

      assert_receive {:received, %ExMonty.Dataclass{name: "Empty", fields: fields, frozen: true}}
      assert fields == %{}
    end
  end

  describe "dataclass method_call dispatch" do
    test "method call dispatched through sandbox" do
      functions = %{
        "make_counter" => fn _args, _kwargs ->
          {:ok,
           %ExMonty.Dataclass{
             name: "Counter",
             fields: %{"value" => 0},
             frozen: false
           }}
        end,
        "increment" => fn [counter], _kwargs ->
          new_val = counter.fields["value"] + 1
          {:ok, %{counter | fields: %{counter.fields | "value" => new_val}}}
        end
      }

      {:ok, result, _output} =
        ExMonty.Sandbox.run(
          "c = make_counter()\nc2 = c.increment()\nc2.value",
          functions: functions
        )

      assert result == 1
    end

    test "method call with arguments" do
      functions = %{
        "make_calc" => fn _args, _kwargs ->
          {:ok,
           %ExMonty.Dataclass{
             name: "Calc",
             fields: %{"value" => 10},
             frozen: false
           }}
        end,
        "add" => fn [self_dc, n], _kwargs ->
          {:ok,
           %ExMonty.Dataclass{
             name: "Calc",
             fields: %{"value" => self_dc.fields["value"] + n},
             frozen: false
           }}
        end
      }

      {:ok, result, _output} =
        ExMonty.Sandbox.run(
          "c = make_calc()\nc2 = c.add(5)\nc2.value",
          functions: functions
        )

      assert result == 15
    end
  end
end
