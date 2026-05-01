defmodule ExMonty.MountSandboxTest do
  @moduledoc """
  Mount-aware `Sandbox.run` tests: mode behaviour, routing, composition
  with `:os` fallbacks, and unmounted-path semantics.
  """
  use ExUnit.Case, async: false

  alias ExMonty.Mount

  setup do
    tmp =
      Path.join(System.tmp_dir!(), "ex_monty_mount_sb_#{:erlang.unique_integer([:positive])}")

    File.mkdir_p!(tmp)
    on_exit(fn -> File.rm_rf!(tmp) end)
    %{tmp: tmp}
  end

  describe ":read_only mode" do
    test "reads from the host pass through", %{tmp: tmp} do
      File.write!(Path.join(tmp, "hello.txt"), "hello world")

      mounts = Mount.new!() |> Mount.add!("/data", tmp, :read_only)

      code = """
      from pathlib import Path
      Path("/data/hello.txt").read_text()
      """

      assert {:ok, "hello world", ""} = ExMonty.Sandbox.run(code, mounts: mounts)
    end

    test "writes raise PermissionError", %{tmp: tmp} do
      mounts = Mount.new!() |> Mount.add!("/data", tmp, :read_only)

      code = """
      from pathlib import Path
      Path("/data/x.txt").write_text("nope")
      """

      assert {:error, %ExMonty.Exception{type: :permission_error}} =
               ExMonty.Sandbox.run(code, mounts: mounts)

      refute File.exists?(Path.join(tmp, "x.txt"))
    end
  end

  describe ":read_write mode" do
    test "writes hit the host", %{tmp: tmp} do
      mounts = Mount.new!() |> Mount.add!("/rw", tmp, :read_write)

      code = """
      from pathlib import Path
      Path("/rw/written.txt").write_text("real")
      """

      assert {:ok, _, _} = ExMonty.Sandbox.run(code, mounts: mounts)
      assert File.read!(Path.join(tmp, "written.txt")) == "real"
    end

    test "reads see host writes", %{tmp: tmp} do
      File.write!(Path.join(tmp, "data.txt"), "from host")
      mounts = Mount.new!() |> Mount.add!("/rw", tmp, :read_write)

      code = """
      from pathlib import Path
      Path("/rw/data.txt").read_text()
      """

      assert {:ok, "from host", ""} = ExMonty.Sandbox.run(code, mounts: mounts)
    end
  end

  describe ":overlay mode" do
    test "writes are captured in-memory; host is untouched", %{tmp: tmp} do
      mounts = Mount.new!() |> Mount.add!("/o", tmp, :overlay)

      code = """
      from pathlib import Path
      p = Path("/o/scratch.txt")
      p.write_text("ephemeral")
      p.read_text()
      """

      assert {:ok, "ephemeral", _} = ExMonty.Sandbox.run(code, mounts: mounts)
      refute File.exists?(Path.join(tmp, "scratch.txt"))
    end

    test "host reads still pass through under overlay", %{tmp: tmp} do
      File.write!(Path.join(tmp, "real.txt"), "from host")
      mounts = Mount.new!() |> Mount.add!("/o", tmp, :overlay)

      code = """
      from pathlib import Path
      Path("/o/real.txt").read_text()
      """

      assert {:ok, "from host", ""} = ExMonty.Sandbox.run(code, mounts: mounts)
    end

    test "overlay writes persist across runs against the same mount", %{tmp: tmp} do
      mounts = Mount.new!() |> Mount.add!("/o", tmp, :overlay)

      write_code = """
      from pathlib import Path
      Path("/o/persist.txt").write_text("first run")
      """

      read_code = """
      from pathlib import Path
      Path("/o/persist.txt").read_text()
      """

      assert {:ok, _, _} = ExMonty.Sandbox.run(write_code, mounts: mounts)
      assert {:ok, "first run", ""} = ExMonty.Sandbox.run(read_code, mounts: mounts)

      refute File.exists?(Path.join(tmp, "persist.txt"))
    end

    test "fresh mount discards overlay state", %{tmp: tmp} do
      m1 = Mount.new!() |> Mount.add!("/o", tmp, :overlay)

      write_code = """
      from pathlib import Path
      Path("/o/foo.txt").write_text("only in m1")
      """

      ExMonty.Sandbox.run(write_code, mounts: m1)

      m2 = Mount.new!() |> Mount.add!("/o", tmp, :overlay)

      read_code = """
      from pathlib import Path
      Path("/o/foo.txt").read_text()
      """

      assert {:error, %ExMonty.Exception{type: :file_not_found_error}} =
               ExMonty.Sandbox.run(read_code, mounts: m2)
    end
  end

  describe "routing" do
    test "longest-prefix wins", %{tmp: tmp} do
      data = Path.join(tmp, "data")
      data_users = Path.join(data, "users")
      File.mkdir_p!(data)
      File.mkdir_p!(data_users)
      File.write!(Path.join(data_users, "alice.txt"), "from data/users mount")

      mounts =
        Mount.new!()
        |> Mount.add!("/data", data, :overlay)
        |> Mount.add!("/data/users", data_users, :read_only)

      code = """
      from pathlib import Path
      Path("/data/users/alice.txt").read_text()
      """

      assert {:ok, "from data/users mount", _} = ExMonty.Sandbox.run(code, mounts: mounts)
    end

    test "unmounted path returns PermissionError via on_no_handler", %{tmp: tmp} do
      mounts = Mount.new!() |> Mount.add!("/data", tmp, :read_only)

      code = """
      from pathlib import Path
      Path("/etc/passwd").read_text()
      """

      assert {:error, %ExMonty.Exception{type: :permission_error, message: msg}} =
               ExMonty.Sandbox.run(code, mounts: mounts)

      assert msg =~ "Permission denied"
    end

    test "cross-mount rename is rejected", %{tmp: tmp} do
      a = Path.join(tmp, "a")
      b = Path.join(tmp, "b")
      File.mkdir_p!(a)
      File.mkdir_p!(b)
      File.write!(Path.join(a, "src.txt"), "x")

      mounts =
        Mount.new!()
        |> Mount.add!("/a", a, :read_write)
        |> Mount.add!("/b", b, :read_write)

      code = """
      from pathlib import Path
      Path("/a/src.txt").rename("/b/dst.txt")
      """

      assert {:error, %ExMonty.Exception{type: type}} =
               ExMonty.Sandbox.run(code, mounts: mounts)

      # Upstream raises OSError. snake_case conversion produces `:o_s_error`
      # (pre-existing behaviour for consecutive-capital exception names —
      # see ExMonty.Exception type encoding).
      assert type in [:os_error, :o_s_error, :runtime_error]
    end
  end

  describe "composition with :os fallbacks" do
    test "mounts + getenv function-map handler", %{tmp: tmp} do
      File.write!(Path.join(tmp, "config.txt"), "the data")
      mounts = Mount.new!() |> Mount.add!("/data", tmp, :read_only)

      getenv_handler = fn _args, _kwargs -> {:ok, "from-host"} end

      code = """
      import os
      from pathlib import Path
      data = Path("/data/config.txt").read_text()
      env = os.getenv("MY_VAR")
      (data, env)
      """

      assert {:ok, {"the data", "from-host"}, ""} =
               ExMonty.Sandbox.run(code, mounts: mounts, os: %{getenv: getenv_handler})
    end

    test "mounts + datetime_now handler", %{tmp: tmp} do
      mounts = Mount.new!() |> Mount.add!("/x", tmp, :read_only)

      now_handler = fn _args, _kwargs ->
        {:ok,
         {:datetime,
          %{
            year: 2026,
            month: 5,
            day: 1,
            hour: 9,
            minute: 0,
            second: 0,
            microsecond: 0,
            offset_seconds: nil,
            tz_name: nil
          }}}
      end

      code = "from datetime import datetime\ndatetime.now().year"

      assert {:ok, 2026, ""} =
               ExMonty.Sandbox.run(code, mounts: mounts, os: %{datetime_now: now_handler})
    end

    test "no fallback for unhandled non-FS op uses on_no_handler RuntimeError", %{tmp: tmp} do
      mounts = Mount.new!() |> Mount.add!("/x", tmp, :read_only)

      code = "import os\nos.getenv(\"FOO\")"

      assert {:error, %ExMonty.Exception{type: :runtime_error, message: msg}} =
               ExMonty.Sandbox.run(code, mounts: mounts)

      assert msg =~ "is not supported in this environment"
    end

    test "mount path wins over PseudoFS when both configured", %{tmp: tmp} do
      File.write!(Path.join(tmp, "shared.txt"), "from mount")
      mounts = Mount.new!() |> Mount.add!("/shared", tmp, :read_only)

      pseudo_fs =
        ExMonty.PseudoFS.new()
        |> ExMonty.PseudoFS.put_file("/shared/shared.txt", "from pseudofs")
        |> ExMonty.PseudoFS.put_file("/elsewhere/data.txt", "from pseudofs only")

      code = """
      from pathlib import Path
      Path("/shared/shared.txt").read_text()
      """

      # Mount intercepts the FS op in Rust before PseudoFS sees it.
      assert {:ok, "from mount", ""} =
               ExMonty.Sandbox.run(code, mounts: mounts, os: pseudo_fs)
    end

    test "PseudoFS handles unmounted paths when both configured", %{tmp: tmp} do
      mounts = Mount.new!() |> Mount.add!("/mounted", tmp, :read_only)

      pseudo_fs =
        ExMonty.PseudoFS.new()
        |> ExMonty.PseudoFS.put_file("/elsewhere/data.txt", "from pseudofs")

      code = """
      from pathlib import Path
      Path("/elsewhere/data.txt").read_text()
      """

      # /elsewhere doesn't match the mount → falls through to PseudoFS.
      assert {:ok, "from pseudofs", ""} =
               ExMonty.Sandbox.run(code, mounts: mounts, os: pseudo_fs)
    end
  end

  describe "Sandbox.run lease lifecycle" do
    test ":mount_in_use returned if mount already leased", %{tmp: tmp} do
      mounts = Mount.new!() |> Mount.add!("/data", tmp, :read_only)

      {:ok, _lease} = Mount.checkout(mounts)

      assert {:error, :mount_in_use} =
               ExMonty.Sandbox.run("1 + 1", mounts: mounts)
    end

    test "lease released after a successful run", %{tmp: tmp} do
      mounts = Mount.new!() |> Mount.add!("/data", tmp, :read_only)

      assert {:ok, _, _} = ExMonty.Sandbox.run("1 + 1", mounts: mounts)

      assert {:ok, _} = Mount.checkout(mounts)
    end

    test "lease released after a Python exception", %{tmp: tmp} do
      mounts = Mount.new!() |> Mount.add!("/data", tmp, :read_only)

      assert {:error, %ExMonty.Exception{type: :zero_division_error}} =
               ExMonty.Sandbox.run("1 / 0", mounts: mounts)

      assert {:ok, _} = Mount.checkout(mounts)
    end
  end
end
