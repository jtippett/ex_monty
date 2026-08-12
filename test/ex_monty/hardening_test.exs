defmodule ExMonty.HardeningTest do
  use ExUnit.Case

  # These tests exercise the NIF's safety boundary: each case feeds the kind of
  # malicious / degenerate input that previously could panic, OOM, or
  # stack-overflow the BEAM, and asserts it now degrades to a clean error (the
  # test process surviving at all is itself the assertion that the VM didn't go
  # down).

  alias ExMonty.PseudoFS

  # The raw NIF either returns `{:error, reason}` or raises `ErlangError`
  # depending on the failure; normalize both to `{:error, _}` so a test can
  # assert "failed cleanly (no VM crash)".
  defp catch_native(fun) do
    case fun.() do
      {:error, _} = err -> err
      other -> {:ok, other}
    end
  rescue
    e in ErlangError -> {:error, e.original}
  end

  describe "nesting-depth guard (decode)" do
    test "a deeply nested input value is rejected, not a stack overflow" do
      {:ok, runner} = ExMonty.compile("x", inputs: ["x"])
      deep = Enum.reduce(1..2_000, 0, fn _, acc -> [acc] end)
      assert {:error, _} = ExMonty.run(runner, %{"x" => deep})
    end

    test "moderately nested input still decodes fine" do
      {:ok, runner} = ExMonty.compile("x", inputs: ["x"])
      ok_deep = Enum.reduce(1..30, 0, fn _, acc -> [acc] end)
      assert {:ok, _value, ""} = ExMonty.run(runner, %{"x" => ok_deep})
    end
  end

  describe "nesting-depth guard (encode)" do
    test "a deeply nested result value is rejected, not a stack overflow" do
      code = """
      x = 0
      for _ in range(2000):
          x = [x]
      x
      """

      assert {:error, _} = ExMonty.eval(code)
    end

    test "moderately nested result still encodes fine" do
      code = """
      x = 0
      for _ in range(30):
          x = [x]
      x
      """

      assert {:ok, _value, ""} = ExMonty.eval(code)
    end
  end

  describe "deep-return recursion safety cap" do
    @deep_return "x = 0\nfor _ in range(50000):\n    x = [x]\nx"

    test "a script returning an ultra-deep value is a clean error, not a VM crash" do
      assert {:error, _} = ExMonty.eval(@deep_return)
    end

    test ":unlimited still enforces the recursion safety cap" do
      assert {:error, _} = ExMonty.eval(@deep_return, limits: :unlimited)
    end

    test "the raw NIF accepts the documented :unlimited marker" do
      {:ok, runner} = ExMonty.compile("1 + 2")
      assert {3, ""} = ExMonty.Native.run(runner, [], :unlimited)
    end

    test "a caller cannot raise max_recursion_depth past the cap" do
      assert ExMonty.max_recursion_depth_cap() <= 300

      assert {:error, _} =
               ExMonty.eval(@deep_return, limits: %{max_recursion_depth: 1_000_000})
    end

    test "the cap is enforced in the NIF, so a raw Native call cannot bypass it" do
      # `nil` limits would otherwise give Monty's default recursion depth of
      # 1000 — deep enough to build a result that overflows on drop. The NIF
      # caps it regardless of how it is called.
      {:ok, runner} = ExMonty.compile(@deep_return, inputs: [])
      assert {:error, _} = catch_native(fn -> ExMonty.Native.run(runner, [], nil) end)
    end

    test "a malformed max_recursion_depth is rejected, not silently coerced" do
      {:ok, runner} = ExMonty.compile("1")
      assert {:error, _} = ExMonty.run(runner, %{}, limits: %{max_recursion_depth: :bad})
    end

    test "a malformed top-level :limits value errors cleanly, not FunctionClauseError" do
      {:ok, runner} = ExMonty.compile("1")
      assert {:error, _} = ExMonty.run(runner, %{}, limits: :bad)
      assert {:error, _} = ExMonty.run(runner, %{}, limits: 123)
    end
  end

  describe "bigint tagged-tuple decoding" do
    setup do
      {:ok, runner} = ExMonty.compile("x", inputs: ["x"])
      %{runner: runner}
    end

    test "a canonical bigint decodes", %{runner: runner} do
      assert {:ok, 1, ""} = ExMonty.run(runner, %{"x" => {:__bigint__, 1, <<1>>}})
      assert {:ok, 0, ""} = ExMonty.run(runner, %{"x" => {:__bigint__, 0, <<>>}})
    end

    test "an out-of-range sign is rejected", %{runner: runner} do
      assert {:error, _} = ExMonty.run(runner, %{"x" => {:__bigint__, 2, <<1>>}})
    end

    test "a non-canonical zero (sign 0 with magnitude) is rejected", %{runner: runner} do
      assert {:error, _} = ExMonty.run(runner, %{"x" => {:__bigint__, 0, <<1>>}})
    end

    test "an over-long magnitude is rejected", %{runner: runner} do
      huge = :binary.copy(<<255>>, 4096)
      assert {:error, _} = ExMonty.run(runner, %{"x" => {:__bigint__, 1, huge}})
    end

    test "ordinary BEAM bigints use the same magnitude cap", %{runner: runner} do
      at_limit = Bitwise.bsl(1, 8 * 1024 - 1)
      over_limit = Bitwise.bsl(1, 8 * 1024)

      assert {:ok, ^at_limit, ""} = ExMonty.run(runner, %{"x" => at_limit})
      assert {:error, _} = ExMonty.run(runner, %{"x" => over_limit})
    end
  end

  describe "dataclass strict decoding" do
    setup do
      {:ok, runner} = ExMonty.compile("x", inputs: ["x"])
      %{runner: runner}
    end

    test "a well-formed dataclass decodes", %{runner: runner} do
      dc = %ExMonty.Dataclass{name: "A", fields: %{"a" => 1}, frozen: false}
      assert {:ok, %ExMonty.Dataclass{}, ""} = ExMonty.run(runner, %{"x" => dc})
    end

    test "a non-boolean frozen is rejected", %{runner: runner} do
      dc = %ExMonty.Dataclass{name: "A", fields: %{}, frozen: :bad}
      assert {:error, _} = ExMonty.run(runner, %{"x" => dc})
    end

    test "a non-integer type_id is rejected", %{runner: runner} do
      dc = %ExMonty.Dataclass{name: "A", fields: %{}, frozen: false, type_id: :bad}
      assert {:error, _} = ExMonty.run(runner, %{"x" => dc})
    end

    test "field_names must match fields exactly", %{runner: runner} do
      missing = %ExMonty.Dataclass{
        name: "A",
        fields: %{"a" => 1, "b" => 2},
        field_names: ["a"],
        frozen: false
      }

      duplicate = %ExMonty.Dataclass{
        name: "A",
        fields: %{"a" => 1},
        field_names: ["a", "a"],
        frozen: false
      }

      assert {:error, _} = ExMonty.run(runner, %{"x" => missing})
      assert {:error, _} = ExMonty.run(runner, %{"x" => duplicate})
    end

    test "named tuple field names must be unique", %{runner: runner} do
      value = {:named_tuple, "Pair", [{"item", 1}, {"item", 2}]}
      assert {:error, _} = ExMonty.run(runner, %{"x" => value})
    end
  end

  describe "default resource limits" do
    test "default_limits/0 exposes the conservative caps" do
      limits = ExMonty.default_limits()
      assert is_map(limits)
      assert limits.max_duration_secs > 0
      assert limits.max_memory > 0
    end

    test "a memory bomb is bounded when :limits is omitted" do
      # ~600 MB string exceeds the default max_memory (512 MB).
      assert {:error, _} = ExMonty.eval("'x' * (600 * 1024 * 1024)")
    end

    test ":unlimited opts out of the default caps for trusted code" do
      assert {:ok, 3, ""} = ExMonty.eval("1 + 2", limits: :unlimited)
    end

    test "captured output counts against max_memory" do
      code = "for _ in range(10_000): print('0123456789')"
      assert {:error, _} = ExMonty.eval(code, limits: %{max_memory: 64 * 1024})
    end
  end

  describe "strict callback result decoding" do
    test "malformed callback tuples do not consume the snapshot" do
      {:ok, runner} = ExMonty.compile("external()")
      {:ok, {:function_call, _call, snapshot, ""}} = ExMonty.start(runner)

      assert {:error, _} = ExMonty.resume(snapshot, {:error, :runtime_error, :bad_message})
      assert {:error, _} = ExMonty.resume(snapshot, {:surprise, 1})
      assert {:error, _} = ExMonty.resume(snapshot, {:error, :invented_error, "bad"})
      assert {:ok, {:complete, 42, ""}} = ExMonty.resume(snapshot, {:ok, 42})
    end

    test "name lookups reject raw values without consuming the snapshot" do
      {:ok, runner} = ExMonty.compile("missing_name")
      {:ok, {:name_lookup, "missing_name", snapshot, ""}} = ExMonty.start(runner)

      assert {:error, _} = ExMonty.resume(snapshot, 42)
      assert {:ok, {:complete, 42, ""}} = ExMonty.resume(snapshot, {:ok, 42})
    end

    test "file handles require a valid position and preserve the snapshot on failure" do
      {:ok, runner} = ExMonty.compile(~s|open("/x", "r")|)
      {:ok, {:os_call, %ExMonty.OsCall{function: :open}, snapshot, ""}} = ExMonty.start(runner)

      bad_handle = {:ok, {:file_handle, %{path: "/x", mode: "r", position: :bad}}}
      assert {:error, _} = ExMonty.resume(snapshot, bad_handle)

      good_handle = {:ok, {:file_handle, %{path: "/x", mode: "r", position: 0}}}

      assert {:ok, {:complete, {:file_handle, %{position: 0}}, ""}} =
               ExMonty.resume(snapshot, good_handle)
    end
  end

  describe "external future lifecycle" do
    test "pending calls are reachable and invalid resolutions preserve the snapshot" do
      code = """
      import asyncio

      async def main():
          a, b = await asyncio.gather(foo(), bar())
          return a + b

      await main()
      """

      {:ok, runner} = ExMonty.compile(code)
      {:ok, progress} = ExMonty.start(runner)
      {futures, call_ids} = drive_to_futures(progress)

      assert length(call_ids) == 2
      assert Enum.sort(ExMonty.pending_call_ids(futures)) == Enum.sort(call_ids)
      [first | _] = call_ids

      assert {:error, _} =
               ExMonty.resume_futures(futures, [{first, {:ok, 1}}, {first, {:ok, 2}}])

      assert {:error, _} = ExMonty.resume_futures(futures, [{4_294_967_295, {:ok, 1}}])
      assert {:error, _} = ExMonty.resume_futures(futures, [{first, {:ok}}])
      assert Enum.sort(ExMonty.pending_call_ids(futures)) == Enum.sort(call_ids)

      assert {:ok, dumped} = ExMonty.dump_future_snapshot(futures)
      assert {:ok, restored} = ExMonty.load_future_snapshot(dumped)
      assert Enum.sort(ExMonty.pending_call_ids(restored)) == Enum.sort(call_ids)

      results =
        call_ids |> Enum.with_index(20) |> Enum.map(fn {id, value} -> {id, {:ok, value}} end)

      assert {:ok, {:complete, 41, ""}} = ExMonty.resume_futures(restored, results)
    end
  end

  describe "resource-limit value validation" do
    setup do
      {:ok, runner} = ExMonty.compile("1")
      %{runner: runner}
    end

    test "a negative duration is rejected, not panicked", %{runner: runner} do
      assert {:error, _} = ExMonty.run(runner, %{}, limits: %{max_duration_secs: -1.0})
    end

    test "a negative memory limit is rejected", %{runner: runner} do
      assert {:error, _} = ExMonty.run(runner, %{}, limits: %{max_memory: -1})
    end
  end

  describe "PseudoFS virtual-path normalization" do
    test "'..' and '.' resolve to the same file" do
      fs = PseudoFS.new() |> PseudoFS.put_file("/data/hello.txt", "hi")

      code = "from pathlib import Path\nPath('/data/../data/./hello.txt').read_text()"
      assert {:ok, "hi", _} = ExMonty.Sandbox.run(code, os: fs)
    end

    test "duplicate slashes resolve to the same file" do
      fs = PseudoFS.new() |> PseudoFS.put_file("/data/hello.txt", "hi")

      code = "from pathlib import Path\nPath('/data//hello.txt').read_text()"
      assert {:ok, "hi", _} = ExMonty.Sandbox.run(code, os: fs)
    end

    test "'..' cannot escape above the root" do
      fs = PseudoFS.new() |> PseudoFS.put_file("/hello.txt", "hi")

      code = "from pathlib import Path\nPath('/../../../hello.txt').read_text()"
      assert {:ok, "hi", _} = ExMonty.Sandbox.run(code, os: fs)
    end
  end

  defp drive_to_futures({:name_lookup, name, snapshot, _output}) do
    {:ok, progress} = ExMonty.resume(snapshot, {:ok, {:function, name}})
    drive_to_futures(progress)
  end

  defp drive_to_futures(progress), do: drive_to_futures(progress, [])

  defp drive_to_futures({:name_lookup, name, snapshot, _output}, call_ids) do
    {:ok, progress} = ExMonty.resume(snapshot, {:ok, {:function, name}})
    drive_to_futures(progress, call_ids)
  end

  defp drive_to_futures(
         {:function_call, %ExMonty.FunctionCall{call_id: id}, snapshot, _output},
         call_ids
       ) do
    {:ok, progress} = ExMonty.resume(snapshot, :pending)
    drive_to_futures(progress, [id | call_ids])
  end

  defp drive_to_futures({:resolve_futures, futures, _output}, call_ids) do
    {futures, Enum.reverse(call_ids)}
  end
end
