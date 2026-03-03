defmodule ExMonty.BuiltinsTest do
  use ExUnit.Case

  describe "filter" do
    test "filter with None removes falsy values" do
      assert {:ok, [1, 2, 3], ""} = ExMonty.eval("list(filter(None, [0, 1, '', 2, None, 3]))")
    end

    test "filter with lambda" do
      assert {:ok, [2, 4], ""} =
               ExMonty.eval("list(filter(lambda x: x % 2 == 0, [1, 2, 3, 4, 5]))")
    end

    test "filter on empty list" do
      assert {:ok, [], ""} = ExMonty.eval("list(filter(None, []))")
    end

    test "filter with user-defined function" do
      code = """
      def is_positive(x):
          return x > 0

      list(filter(is_positive, [-2, -1, 0, 1, 2]))
      """

      assert {:ok, [1, 2], ""} = ExMonty.eval(code)
    end
  end

  describe "map" do
    test "map with abs" do
      assert {:ok, [1, 2, 3], ""} = ExMonty.eval("list(map(abs, [-1, -2, -3]))")
    end

    test "map with str" do
      assert {:ok, ["1", "2", "3"], ""} = ExMonty.eval("list(map(str, [1, 2, 3]))")
    end

    test "map with two iterables" do
      {:ok, result, ""} = ExMonty.eval("list(map(pow, [2, 3], [3, 2]))")
      assert result == [8, 9]
    end

    test "map stops at shortest iterable" do
      {:ok, result, ""} = ExMonty.eval("list(map(pow, [1, 2, 3], [1, 2]))")
      assert result == [1, 4]
    end

    test "map with user-defined function" do
      code = """
      def double(x):
          return x * 2

      list(map(double, [1, 2, 3]))
      """

      assert {:ok, [2, 4, 6], ""} = ExMonty.eval(code)
    end
  end

  describe "getattr" do
    test "getattr on object attribute" do
      code = """
      from pathlib import Path
      getattr(Path('/tmp/test.txt'), 'name')
      """

      assert {:ok, "test.txt", ""} = ExMonty.eval(code)
    end

    test "getattr with default for missing attribute" do
      code = """
      from pathlib import Path
      getattr(Path('/tmp'), 'missing', 'default_val')
      """

      assert {:ok, "default_val", ""} = ExMonty.eval(code)
    end

    test "getattr raises AttributeError without default" do
      code = """
      from pathlib import Path
      getattr(Path('/tmp'), 'missing')
      """

      assert {:error, %ExMonty.Exception{type: :attribute_error}} = ExMonty.eval(code)
    end
  end

  describe "dict constructor" do
    test "dict from list of tuples" do
      assert {:ok, result, ""} = ExMonty.eval("dict([('a', 1), ('b', 2)])")
      assert result["a"] == 1
      assert result["b"] == 2
    end

    test "dict with keyword args" do
      assert {:ok, result, ""} = ExMonty.eval("dict(a=1, b=2)")
      assert result["a"] == 1
      assert result["b"] == 2
    end

    test "dict empty" do
      assert {:ok, %{}, ""} = ExMonty.eval("dict()")
    end
  end
end
