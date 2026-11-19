defmodule ExMonty.V160FeaturesTest do
  @moduledoc """
  Behaviour introduced or changed by the monty v1.6.0 update: new value
  types, host-routed clock/sleep/entropy/import OS calls, directory scans,
  host-object method routing, and the v3 dump format.
  """
  use ExUnit.Case, async: false

  alias ExMonty.{Mount, PseudoFS, Sandbox}

  defp run_type(code, opts \\ []) do
    case Sandbox.run(code, opts) do
      {:error, %ExMonty.Exception{type: type, message: message}} -> {:error, type, message}
      other -> other
    end
  end

  describe "new value types" do
    test "complex numbers cross the boundary both ways" do
      assert {:ok, {:complex, 1.0, 2.0}, ""} = ExMonty.eval("complex(1, 2)")

      assert {:ok, {:complex, 4.0, 6.0}, ""} =
               ExMonty.eval("z * 2", inputs: %{"z" => {:complex, 2.0, 3.0}})
    end

    test "decimals round-trip as strings without losing precision" do
      assert {:ok, {:decimal, "0.3"}, ""} =
               ExMonty.eval("from decimal import Decimal\nDecimal('0.1') + Decimal('0.2')")

      assert {:ok, {:decimal, "2.50"}, ""} =
               ExMonty.eval("d + d", inputs: %{"d" => {:decimal, "1.25"}})
    end

    test "datetime.time values encode as a tagged map and decode back" do
      assert {:ok, {:time, fields} = time, ""} =
               ExMonty.eval("from datetime import time\ntime(12, 30, 45, 7)")

      assert %{hour: 12, minute: 30, second: 45, microsecond: 7} = fields
      assert {:ok, 12, ""} = ExMonty.eval("t.hour", inputs: %{"t" => time})
    end

    test "a self-referential value comes back as a cycle placeholder, not a hang" do
      assert {:ok, [1, {:cycle, placeholder}], ""} = ExMonty.eval("a = [1]\na.append(a)\na")
      assert is_binary(placeholder)
    end

    test "classes, modules and functions surface as reprs" do
      assert {:ok, {:repr, "<class 'P'>"}, ""} = ExMonty.eval("class P:\n    pass\nP")
      assert {:ok, {:repr, "<module 'math'>"}, ""} = ExMonty.eval("import math\nmath")
    end

    test "to_json_safe projects the new tags" do
      assert ExMonty.Value.to_json_safe({:decimal, "1.5"}) == "1.5"
      assert ExMonty.Value.to_json_safe({:cycle, "[...]"}) == "[...]"
      assert ExMonty.Value.to_json_safe({:complex, 1.0, 2.0}) == %{"real" => 1.0, "imag" => 2.0}

      {:ok, time, ""} = ExMonty.eval("from datetime import time\ntime(1, 2, 3)")
      assert ExMonty.Value.to_json_safe(time) == "01:02:03"

      # Whatever comes out must be encodable.
      {:ok, value, ""} =
        ExMonty.eval("from decimal import Decimal\n{'z': complex(1, 2), 'd': Decimal('2')}")

      assert %{"z" => %{"real" => 1.0, "imag" => 2.0}, "d" => "2"} =
               ExMonty.Value.to_json_safe(value)
    end
  end

  describe "output expansion guard" do
    test "a small heap that expands into a huge term is rejected, not materialised" do
      # 60 levels of [x, x] share every child: tiny on the Python heap,
      # 2^60 nodes once expanded into an Elixir term.
      code = """
      x = [0]
      for _ in range(60):
          x = [x, x]
      x
      """

      assert {:error, message} = ExMonty.eval(code)
      assert message =~ "MAX_EXPANDED_NODES"
    end

    test "one large value referenced many times is rejected by its expanded bytes" do
      # Two nodes and a million terms, but ~100 GB once copied between
      # processes: big integers are not shared across a send.
      code = "b = 1 << 800_000\n[b] * 1_000_000"
      assert {:error, message} = ExMonty.eval(code)
      assert message =~ "MAX_EXPANDED_BYTES"

      # The same holds for call arguments, which Sandbox copies to a worker.
      assert {:error, message} =
               ExMonty.Sandbox.run("b = 1 << 800_000\nf([b] * 1_000_000)",
                 functions: %{"f" => fn _args, _kwargs -> {:ok, nil} end}
               )

      assert inspect(message) =~ "MAX_EXPANDED_BYTES"
    end

    test "a large value without sharing is still returned" do
      assert {:ok, value, ""} = ExMonty.eval("'a' * 5_000_000")
      assert byte_size(value) == 5_000_000
    end

    test "arguments that are each acceptable are bounded together" do
      # ~6M expanded terms per argument, 1000 arguments.
      code = """
      x = [0]
      for _ in range(21):
          x = [x, x]
      f(*([x] * 1000))
      """

      assert {:error, message} =
               ExMonty.Sandbox.run(code, functions: %{"f" => fn _a, _k -> {:ok, nil} end})

      assert inspect(message) =~ "MAX_EXPANDED_NODES"
    end
  end

  describe "sandbox-defined class instances" do
    test "surface as an ExMonty.Dataclass with stable ids" do
      code = """
      class P:
          def __init__(self):
              self.v = 1
      P()
      """

      assert {:ok, %ExMonty.Dataclass{name: "P", fields: %{"v" => 1}} = dc, ""} =
               ExMonty.eval(code)

      assert is_binary(dc.id) and is_binary(dc.type_id)
      assert dc.field_names == ["v"]
      assert dc.origin == :sandbox
    end

    test "a dead instance handed back is a data copy that cannot call into the host" do
      # `Account()` is a temporary: by the time the host returns it the
      # original is gone. The sandbox must not be able to turn that into a
      # method call on a host object of a class name it picked itself.
      code = """
      class Account:
          def __init__(self):
              self.balance = 10**9
      r = keep(Account())
      before = r.balance
      r.withdraw()
      """

      test_pid = self()

      handler = %{
        "keep" => fn [x], _ -> {:ok, x} end,
        "withdraw" => fn args, _ ->
          send(test_pid, {:host_called, args})
          {:ok, nil}
        end
      }

      assert {:error, %ExMonty.Exception{}} = Sandbox.run(code, functions: handler)
      refute_received {:host_called, _}

      assert {:ok, 1_000_000_000, ""} =
               Sandbox.run(String.replace(code, "r.withdraw()", "before"), functions: handler)
    end

    test "a live instance handed to the host and back keeps identity and methods" do
      code = """
      class P:
          def __init__(self):
              self.v = 1
          def m(self):
              return self.v + 41
      p = P()
      q = keep(p)
      [q.m(), q is p]
      """

      assert {:ok, [42, true], ""} =
               Sandbox.run(code, functions: %{"keep" => fn [x], _ -> {:ok, x} end})
    end
  end

  describe "host-provided dataclasses" do
    setup do
      %{user: %ExMonty.Dataclass{name: "User", fields: %{"name" => "ada", "age" => 36}}}
    end

    test "method calls reach the host with the instance as first argument", %{user: user} do
      {:ok, runner} = ExMonty.compile("u.greet(1, k=2)", inputs: ["u"])

      assert {:ok, {:method_call, call, snapshot, ""}} = ExMonty.start(runner, %{"u" => user})
      assert call.name == "greet"
      assert [%ExMonty.Dataclass{name: "User", fields: %{"name" => "ada"}}, 1] = call.args
      assert call.kwargs == %{"k" => 2}

      assert {:ok, {:complete, "hi", ""}} = ExMonty.resume(snapshot, {:ok, "hi"})
    end

    test "the receiver survives a snapshot dump/load round trip", %{user: user} do
      {:ok, runner} = ExMonty.compile("r = fetch()\nu.greet(r)", inputs: ["u"])
      {:ok, {:function_call, _call, snap, ""}} = ExMonty.start(runner, %{"u" => user})

      {:ok, binary} = ExMonty.dump_snapshot(snap)
      {:ok, restored} = ExMonty.load_snapshot(binary)

      assert {:ok, {:method_call, call, _snap, ""}} = ExMonty.resume(restored, {:ok, 7})
      assert [%ExMonty.Dataclass{name: "User"}, 7] = call.args
    end

    test "reading a missing attribute raises AttributeError without a host round trip",
         %{user: user} do
      assert {:error, :attribute_error, _} = run_type("u.nope", inputs: %{"u" => user})

      assert {:ok, [true, false, 5], ""} =
               Sandbox.run("[hasattr(u, 'name'), hasattr(u, 'nope'), getattr(u, 'nope', 5)]",
                 inputs: %{"u" => user}
               )
    end

    test "host objects are marked as such on the way out", %{user: user} do
      assert {:ok, %ExMonty.Dataclass{origin: :host}, ""} =
               ExMonty.eval("u", inputs: %{"u" => user})

      assert {:error, _} = ExMonty.eval("u", inputs: %{"u" => %{user | origin: :other}})
    end

    test "calling the instance itself is a TypeError", %{user: user} do
      assert {:error, :type_error, message} = run_type("u()", inputs: %{"u" => user})
      assert message =~ "not callable"
    end

    test "repr and equality use the fields", %{user: user} do
      assert {:ok, [repr, true], ""} =
               Sandbox.run("[repr(a), a == b]", inputs: %{"a" => user, "b" => user})

      assert repr =~ "User("
      assert repr =~ "name='ada'"
    end

    test "string type_id and id are accepted and echoed back", %{user: user} do
      id = "00000000-0000-4000-8000-000000000001"
      type_id = "00000000-0000-4000-8000-000000000002"
      user = %{user | id: id, type_id: type_id}

      assert {:ok, %ExMonty.Dataclass{id: ^id, type_id: ^type_id}, ""} =
               ExMonty.eval("u", inputs: %{"u" => user})
    end

    test "a malformed uuid is rejected", %{user: user} do
      assert {:error, _} = ExMonty.eval("u", inputs: %{"u" => %{user | id: "not-a-uuid"}})
    end
  end

  describe "name lookup" do
    test "can be answered with a specific exception" do
      {:ok, runner} = ExMonty.compile("missing")
      {:ok, {:name_lookup, "missing", snapshot, ""}} = ExMonty.start(runner)

      assert {:error, %ExMonty.Exception{type: :value_error, message: "nope"}} =
               ExMonty.resume(snapshot, {:error, :value_error, "nope"})
    end
  end

  describe "host-routed OS calls" do
    test "the clock is never read from the host implicitly" do
      assert {:error, :o_s_error, message} = run_type("import time\ntime.time()")
      assert message =~ "not permitted"

      assert {:error, %ExMonty.Exception{type: :not_implemented_error}} =
               ExMonty.eval("import time\ntime.time()")
    end

    test "time.time() and time.monotonic() ask the :time handler which clock they want" do
      test_pid = self()

      os = %{
        time: fn [which], _kwargs ->
          send(test_pid, {:clock, which})
          {:ok, 12.5}
        end
      }

      assert {:ok, [12.5, 12.5], ""} =
               Sandbox.run("import time\n[time.time(), time.monotonic()]", os: os)

      assert_received {:clock, "time.time"}
      assert_received {:clock, "time.monotonic"}
    end

    test "time.sleep() is delegated to the host and never blocks the scheduler itself" do
      test_pid = self()

      os = %{
        sleep: fn [seconds], _kwargs ->
          send(test_pid, {:slept, seconds})
          {:ok, nil}
        end
      }

      assert {:ok, nil, ""} = Sandbox.run("import time\ntime.sleep(3600)", os: os)
      assert_received {:slept, 3600.0}
      assert {:error, :o_s_error, _} = run_type("import time\ntime.sleep(3600)")
    end

    test "asyncio.sleep() surfaces as :async_sleep" do
      code = """
      import asyncio
      async def f():
          await asyncio.sleep(0.25)
          return 3
      await f()
      """

      test_pid = self()

      os = %{
        async_sleep: fn [seconds], _kwargs ->
          send(test_pid, {:slept, seconds})
          {:ok, nil}
        end
      }

      assert {:ok, 3, ""} = Sandbox.run(code, os: os)
      assert_received {:slept, 0.25}
    end

    test "an :async_sleep os call can be left pending and resolved as a future" do
      code = """
      import asyncio
      async def f():
          await asyncio.sleep(0.25)
          return 3
      await f()
      """

      {:ok, runner} = ExMonty.compile(code)

      assert {:ok, {:os_call, %ExMonty.OsCall{function: :async_sleep} = call, snapshot, ""}} =
               ExMonty.start(runner)

      assert {:ok, {:resolve_futures, futures, ""}} = ExMonty.resume(snapshot, :pending)
      assert ExMonty.pending_call_ids(futures) == [call.call_id]

      assert {:ok, {:complete, 3, ""}} =
               ExMonty.resume_futures(futures, [{call.call_id, {:ok, nil}}])
    end

    test ":pending is refused for an os call that cannot be awaited" do
      {:ok, runner} = ExMonty.compile("import time\ntime.time()")
      {:ok, {:os_call, %ExMonty.OsCall{function: :time}, snapshot, ""}} = ExMonty.start(runner)

      assert {:error, _} = ExMonty.resume(snapshot, :pending)
      # A refused result does not consume the snapshot.
      assert {:ok, {:complete, 1.0, ""}} = ExMonty.resume(snapshot, {:ok, 1.0})
    end

    test "os.urandom() takes its bytes from the host and checks the length" do
      os = %{urandom: fn [n], _kwargs -> {:ok, {:bytes, :binary.copy(<<7>>, n)}} end}
      assert {:ok, {:bytes, <<7, 7, 7, 7>>}, ""} = Sandbox.run("import os\nos.urandom(4)", os: os)

      short = %{urandom: fn _args, _kwargs -> {:ok, {:bytes, <<1>>}} end}
      assert {:error, :runtime_error, message} = run_type("import os\nos.urandom(4)", os: short)
      assert message =~ "expected 4"

      assert {:error, :o_s_error, _} = run_type("import os\nos.urandom(4)")
    end

    test "an unknown import is a ModuleNotFoundError" do
      assert {:error, :module_not_found_error, "No module named 'nosuch'"} =
               run_type("import nosuch")

      assert {:error, %ExMonty.Exception{type: :module_not_found_error}} =
               ExMonty.eval("import nosuch")
    end

    test ":no_handler in plain resume/2 raises the operation's default error" do
      {:ok, runner} = ExMonty.compile("import nosuch")

      assert {:ok, {:os_call, %ExMonty.OsCall{function: :import, args: ["nosuch" | _]}, snap, ""}} =
               ExMonty.start(runner)

      assert {:error, %ExMonty.Exception{type: :module_not_found_error}} =
               ExMonty.resume(snap, :no_handler)
    end

    test ":no_handler is refused for a function call" do
      {:ok, runner} = ExMonty.compile("f()")
      {:ok, {:function_call, _call, snap, ""}} = ExMonty.start(runner)
      assert {:error, _} = ExMonty.resume(snap, :no_handler)
    end
  end

  describe "directory scans on PseudoFS" do
    setup do
      fs =
        PseudoFS.new()
        |> PseudoFS.put_file("/d/a.txt", "x")
        |> PseudoFS.put_file("/d/sub/b.py", "y")
        |> PseudoFS.put_file("/d/sub/deep/c.txt", "z")

      %{fs: fs}
    end

    test "Path.glob and Path.rglob", %{fs: fs} do
      assert {:ok, ["/d/a.txt", "/d/sub"], ""} =
               Sandbox.run(
                 "from pathlib import Path\nsorted(str(p) for p in Path('/d').glob('*'))",
                 os: fs
               )

      assert {:ok, ["/d/a.txt", "/d/sub/deep/c.txt"], ""} =
               Sandbox.run(
                 "from pathlib import Path\nsorted(str(p) for p in Path('/d').rglob('*.txt'))",
                 os: fs
               )
    end

    test "os.walk and os.scandir", %{fs: fs} do
      assert {:ok,
              [
                {"/d", ["sub"], ["a.txt"]},
                {"/d/sub", ["deep"], ["b.py"]},
                {"/d/sub/deep", [], ["c.txt"]}
              ], ""} =
               Sandbox.run(
                 "import os\nsorted((r, sorted(d), sorted(f)) for r, d, f in os.walk('/d'))",
                 os: fs
               )

      assert {:ok, [{"a.txt", false}, {"sub", true}], ""} =
               Sandbox.run(
                 "import os\nsorted((e.name, e.is_dir()) for e in os.scandir('/d'))",
                 os: fs
               )
    end

    test "scanning a missing directory or a file raises", %{fs: fs} do
      assert {:error, :file_not_found_error, _} =
               run_type("import os\nlist(os.scandir('/nope'))", os: fs)

      assert {:error, :not_a_directory_error, _} =
               run_type("import os\nlist(os.scandir('/d/a.txt'))", os: fs)
    end
  end

  describe "mounts" do
    setup do
      tmp = Path.join(System.tmp_dir!(), "ex_monty_v160_#{:erlang.unique_integer([:positive])}")
      File.mkdir_p!(tmp)
      on_exit(fn -> File.rm_rf!(tmp) end)
      %{tmp: tmp}
    end

    test "two mounts may not share or nest host directories", %{tmp: tmp} do
      sub = Path.join(tmp, "sub")
      File.mkdir_p!(sub)
      m = Mount.new!() |> Mount.add!("/a", tmp, :read_only)

      assert {:error, :overlapping_mounts} = Mount.add(m, "/b", tmp, :read_only)
      assert {:error, :overlapping_mounts} = Mount.add(m, "/c", sub, :read_write)
      assert Mount.count(m) == 1
    end

    test "glob and walk are served by the mount and cannot follow a symlink out", %{tmp: tmp} do
      root = Path.join(tmp, "mounted")
      outside = Path.join(tmp, "outside")
      File.mkdir_p!(Path.join(root, "sub"))
      File.mkdir_p!(outside)
      File.write!(Path.join(root, "a.txt"), "a")
      File.write!(Path.join([root, "sub", "b.txt"]), "b")
      File.write!(Path.join(outside, "secret.txt"), "do not leak")
      :ok = File.ln_s(outside, Path.join(root, "leak"))

      mounts = Mount.new!() |> Mount.add!("/m", root, :read_only)

      code = """
      import os
      from pathlib import Path
      globbed = sorted(str(p) for p in Path('/m').rglob('*.txt'))
      walked = sorted(f for _, _, files in os.walk('/m', followlinks=True) for f in files)
      [globbed, walked]
      """

      assert {:ok, [globbed, walked], ""} = Sandbox.run(code, mounts: mounts)
      assert globbed == ["/m/a.txt", "/m/sub/b.txt"]
      assert "secret.txt" not in walked
    end

    test "mount matching is component-aware", %{tmp: tmp} do
      data = Path.join(tmp, "data")
      File.mkdir_p!(data)
      File.write!(Path.join(data, "x.txt"), "x")
      mounts = Mount.new!() |> Mount.add!("/data", data, :read_only)

      assert {:error, %ExMonty.Exception{}} =
               Sandbox.run("from pathlib import Path\nPath('/database/x.txt').read_text()",
                 mounts: mounts
               )
    end
  end

  describe "serialization format" do
    test "dumps carry the v3 headers" do
      {:ok, runner} = ExMonty.compile("f()")
      {:ok, <<"EXMR", 3, _::binary>>} = ExMonty.dump(runner)

      {:ok, {:function_call, _call, snapshot, ""}} = ExMonty.start(runner)
      assert {:ok, <<"EXMS", 3, _::binary>>} = ExMonty.dump_snapshot(snapshot)
    end

    test "binaries written by an older format are rejected, not misread" do
      {:ok, runner} = ExMonty.compile("1 + 1")
      {:ok, <<"EXMR", 3, rest::binary>>} = ExMonty.dump(runner)

      assert {:error, _} = ExMonty.load_runner(<<"EXMR", 2, rest::binary>>)
      assert {:error, _} = ExMonty.load_snapshot(<<"EXMS", 2, rest::binary>>)
    end
  end
end
