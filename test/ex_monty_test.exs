defmodule ExMontyTest do
  use ExUnit.Case

  describe "eval/2" do
    test "simple arithmetic" do
      assert {:ok, 4, ""} = ExMonty.eval("2 + 2")
    end

    test "string operations" do
      assert {:ok, "hello world", ""} = ExMonty.eval("'hello' + ' ' + 'world'")
    end

    test "list comprehension" do
      assert {:ok, [0, 1, 4, 9, 16], ""} = ExMonty.eval("[x**2 for x in range(5)]")
    end

    test "with inputs" do
      assert {:ok, 30, ""} = ExMonty.eval("x + y", inputs: %{"x" => 10, "y" => 20})
    end

    test "none result" do
      assert {:ok, nil, ""} = ExMonty.eval("None")
    end

    test "boolean result" do
      assert {:ok, true, ""} = ExMonty.eval("True")
      assert {:ok, false, ""} = ExMonty.eval("False")
    end

    test "float result" do
      assert {:ok, 3.14, ""} = ExMonty.eval("3.14")
    end

    test "print output" do
      assert {:ok, nil, "hello\n"} = ExMonty.eval("print('hello')")
    end

    test "multi-line code" do
      code = """
      x = 10
      y = 20
      x + y
      """

      assert {:ok, 30, ""} = ExMonty.eval(code)
    end

    test "function definition and call" do
      code = """
      def add(a, b):
          return a + b

      add(3, 4)
      """

      assert {:ok, 7, ""} = ExMonty.eval(code)
    end

    test "syntax error returns error" do
      assert {:error, %ExMonty.Exception{type: :syntax_error}} = ExMonty.eval("def")
    end

    test "runtime error returns error" do
      assert {:error, %ExMonty.Exception{}} = ExMonty.eval("1 / 0")
    end

    test "name error returns error" do
      assert {:error, %ExMonty.Exception{type: :name_error}} = ExMonty.eval("undefined_var")
    end
  end

  describe "compile/2 and run/3" do
    test "compile and run separately" do
      {:ok, runner} = ExMonty.compile("x * 2", inputs: ["x"])
      assert {:ok, 10, ""} = ExMonty.run(runner, %{"x" => 5})
    end

    test "runner is reusable" do
      {:ok, runner} = ExMonty.compile("x + 1", inputs: ["x"])
      assert {:ok, 2, ""} = ExMonty.run(runner, %{"x" => 1})
      assert {:ok, 11, ""} = ExMonty.run(runner, %{"x" => 10})
      assert {:ok, 101, ""} = ExMonty.run(runner, %{"x" => 100})
    end

    test "with resource limits" do
      {:ok, runner} = ExMonty.compile("x + 1", inputs: ["x"])

      assert {:ok, 2, ""} =
               ExMonty.run(runner, %{"x" => 1}, limits: %{max_duration_secs: 5.0})
    end

    test "no inputs" do
      {:ok, runner} = ExMonty.compile("42")
      assert {:ok, 42, ""} = ExMonty.run(runner)
    end
  end

  describe "type mapping" do
    test "integer" do
      assert {:ok, 42, ""} = ExMonty.eval("42")
    end

    test "negative integer" do
      assert {:ok, -42, ""} = ExMonty.eval("-42")
    end

    test "float" do
      assert {:ok, 3.14, ""} = ExMonty.eval("3.14")
    end

    test "string" do
      assert {:ok, "hello", ""} = ExMonty.eval("'hello'")
    end

    test "empty string" do
      assert {:ok, "", ""} = ExMonty.eval("''")
    end

    test "none" do
      assert {:ok, nil, ""} = ExMonty.eval("None")
    end

    test "boolean true" do
      assert {:ok, true, ""} = ExMonty.eval("True")
    end

    test "boolean false" do
      assert {:ok, false, ""} = ExMonty.eval("False")
    end

    test "ellipsis" do
      assert {:ok, :ellipsis, ""} = ExMonty.eval("...")
    end

    test "list" do
      assert {:ok, [1, 2, 3], ""} = ExMonty.eval("[1, 2, 3]")
    end

    test "empty list" do
      assert {:ok, [], ""} = ExMonty.eval("[]")
    end

    test "nested list" do
      assert {:ok, [[1, 2], [3, 4]], ""} = ExMonty.eval("[[1, 2], [3, 4]]")
    end

    test "tuple" do
      assert {:ok, {1, 2, 3}, ""} = ExMonty.eval("(1, 2, 3)")
    end

    test "dict" do
      assert {:ok, result, ""} = ExMonty.eval("{'a': 1, 'b': 2}")
      assert result["a"] == 1
      assert result["b"] == 2
    end

    test "empty dict" do
      assert {:ok, result, ""} = ExMonty.eval("{}")
      assert result == %{}
    end

    test "set" do
      assert {:ok, result, ""} = ExMonty.eval("{1, 2, 3}")
      assert is_struct(result, MapSet) or is_map(result)
    end

    test "bytes input roundtrip" do
      {:ok, runner} = ExMonty.compile("x", inputs: ["x"])
      assert {:ok, {:bytes, <<1, 2, 3>>}, ""} = ExMonty.run(runner, %{"x" => {:bytes, <<1, 2, 3>>}})
    end

    test "path input roundtrip" do
      code = """
      from pathlib import Path
      Path('/tmp/test.txt')
      """

      assert {:ok, {:path, "/tmp/test.txt"}, ""} = ExMonty.eval(code)
    end

    test "list input" do
      {:ok, runner} = ExMonty.compile("len(x)", inputs: ["x"])
      assert {:ok, 3, ""} = ExMonty.run(runner, %{"x" => [1, 2, 3]})
    end

    test "dict input" do
      {:ok, runner} = ExMonty.compile("x['key']", inputs: ["x"])
      assert {:ok, 42, ""} = ExMonty.run(runner, %{"x" => %{"key" => 42}})
    end

    test "mixed type list" do
      assert {:ok, result, ""} = ExMonty.eval("[1, 'two', 3.0, True, None]")
      assert result == [1, "two", 3.0, true, nil]
    end
  end

  describe "print capture" do
    test "single print" do
      assert {:ok, nil, "hello\n"} = ExMonty.eval("print('hello')")
    end

    test "multiple prints" do
      code = """
      print('line 1')
      print('line 2')
      """

      assert {:ok, nil, "line 1\nline 2\n"} = ExMonty.eval(code)
    end

    test "print with value" do
      code = """
      print('computing...')
      2 + 2
      """

      assert {:ok, 4, "computing...\n"} = ExMonty.eval(code)
    end

    test "formatted print" do
      assert {:ok, nil, output} = ExMonty.eval("print(f'{1 + 2} items')")
      assert output == "3 items\n"
    end
  end

  describe "error handling" do
    test "zero division" do
      assert {:error, %ExMonty.Exception{type: :zero_division_error}} = ExMonty.eval("1 / 0")
    end

    test "type error" do
      assert {:error, %ExMonty.Exception{type: :type_error}} = ExMonty.eval("'a' + 1")
    end

    test "index error" do
      assert {:error, %ExMonty.Exception{type: :index_error}} = ExMonty.eval("[1, 2][5]")
    end

    test "key error" do
      assert {:error, %ExMonty.Exception{type: :key_error}} = ExMonty.eval("{}['missing']")
    end

    test "attribute error" do
      assert {:error, %ExMonty.Exception{type: :attribute_error}} =
               ExMonty.eval("(1).nonexistent")
    end

    test "exception has message" do
      {:error, exc} = ExMonty.eval("raise ValueError('test message')")
      assert exc.type == :value_error
      assert exc.message =~ "test message"
    end

    test "exception has traceback" do
      {:error, exc} = ExMonty.eval("1 / 0")
      assert is_list(exc.traceback)
    end
  end
end
