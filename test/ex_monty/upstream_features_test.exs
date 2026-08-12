defmodule ExMonty.UpstreamFeaturesTest do
  use ExUnit.Case, async: false

  # Capabilities added upstream between monty v0.0.18 and v0.0.21.

  describe "user-defined classes" do
    test "class with methods and attributes" do
      code = """
      class Counter:
          def __init__(self, start):
              self.value = start

          def bump(self, by):
              self.value = self.value + by
              return self.value

      c = Counter(10)
      c.bump(5)
      c.bump(7)
      """

      assert {:ok, 22, ""} = ExMonty.eval(code)
    end

    test "user-defined __iter__/__next__ drive a for loop" do
      code = """
      class UpTo:
          def __init__(self, n):
              self.n = n
              self.i = 0

          def __iter__(self):
              return self

          def __next__(self):
              if self.i >= self.n:
                  raise StopIteration
              self.i = self.i + 1
              return self.i

      total = 0
      for x in UpTo(4):
          total = total + x
      total
      """

      assert {:ok, 10, ""} = ExMonty.eval(code)
    end
  end

  describe "decorators" do
    test "function decorator" do
      code = """
      def double_result(f):
          def wrapper(x):
              return f(x) * 2
          return wrapper

      @double_result
      def add_one(x):
          return x + 1

      add_one(5)
      """

      assert {:ok, 12, ""} = ExMonty.eval(code)
    end
  end

  describe "in-sandbox dataclasses" do
    test "@dataclass defined and used inside the sandbox" do
      code = """
      from dataclasses import dataclass

      @dataclass
      class Point:
          x: int
          y: int

      p = Point(3, 4)
      p.x + p.y
      """

      assert {:ok, 7, ""} = ExMonty.eval(code)
    end

    test "an in-sandbox dataclass instance surfaces as a repr" do
      code = """
      from dataclasses import dataclass

      @dataclass
      class Point:
          x: int
          y: int

      Point(3, 4)
      """

      # Instances of classes defined *inside* the sandbox don't map to
      # %ExMonty.Dataclass{} (that variant is for host-provided instances);
      # they cross the boundary as their repr.
      assert {:ok, {:repr, "Point(x=3, y=4)"}, ""} = ExMonty.eval(code)
    end
  end

  describe "collections module" do
    test "Counter" do
      code = """
      from collections import Counter

      Counter("abracadabra").most_common(1)
      """

      assert {:ok, [{"a", 5}], ""} = ExMonty.eval(code)
    end

    test "deque" do
      code = """
      from collections import deque

      d = deque([1, 2, 3])
      d.appendleft(0)
      d.append(4)
      list(d)
      """

      assert {:ok, [0, 1, 2, 3, 4], ""} = ExMonty.eval(code)
    end

    test "defaultdict" do
      code = """
      from collections import defaultdict

      d = defaultdict(int)
      d["x"] = d["x"] + 1
      d["x"] = d["x"] + 1
      d["x"]
      """

      assert {:ok, 2, ""} = ExMonty.eval(code)
    end

    test "namedtuple" do
      code = """
      from collections import namedtuple

      Point = namedtuple("Point", ["x", "y"])
      Point(1, 2)
      """

      assert {:ok, {:named_tuple, "Point", [{"x", 1}, {"y", 2}]}, ""} = ExMonty.eval(code)
    end
  end

  describe "itertools module" do
    test "count + islice" do
      code = """
      import itertools

      list(itertools.islice(itertools.count(5), 3))
      """

      assert {:ok, [5, 6, 7], ""} = ExMonty.eval(code)
    end

    test "chain and pairwise" do
      code = """
      import itertools

      list(itertools.pairwise(itertools.chain([1, 2], [3])))
      """

      assert {:ok, [{1, 2}, {2, 3}], ""} = ExMonty.eval(code)
    end
  end

  describe "misc language improvements" do
    test "iter(callable, sentinel)" do
      code = """
      values = [1, 2, 0, 3]

      def pop():
          return values.pop(0)

      list(iter(pop, 0))
      """

      assert {:ok, [1, 2], ""} = ExMonty.eval(code)
    end

    test "pytest-style assert failure message" do
      assert {:error, %ExMonty.Exception{type: :assertion_error}} =
               ExMonty.eval("assert 1 + 1 == 3")
    end

    test "star-unpacking any iterable" do
      code = """
      def f(a, b, c):
          return a + b + c

      f(*range(3))
      """

      assert {:ok, 3, ""} = ExMonty.eval(code)
    end
  end
end
