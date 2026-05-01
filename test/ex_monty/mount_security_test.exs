defmodule ExMonty.MountSecurityTest do
  @moduledoc """
  Security tests for `ExMonty.Mount` — symlink-escape, `..` traversal,
  invalid-path validation. These exercise upstream monty's path-security
  layer through ExMonty's API.
  """
  use ExUnit.Case, async: false

  alias ExMonty.Mount

  setup do
    tmp =
      Path.join(System.tmp_dir!(), "ex_monty_mount_sec_#{:erlang.unique_integer([:positive])}")

    File.mkdir_p!(tmp)
    on_exit(fn -> File.rm_rf!(tmp) end)
    %{tmp: tmp}
  end

  describe "symlink escape blocked" do
    test "absolute symlink outside the mount root is rejected", %{tmp: tmp} do
      mount_root = Path.join(tmp, "mounted")
      outside = Path.join(tmp, "outside")
      File.mkdir_p!(mount_root)
      File.mkdir_p!(outside)
      File.write!(Path.join(outside, "secret.txt"), "do not leak")

      # Create a symlink inside the mount that points outside.
      :ok = File.ln_s(Path.join(outside, "secret.txt"), Path.join(mount_root, "leak"))

      mounts = Mount.new!() |> Mount.add!("/m", mount_root, :read_only)

      code = """
      from pathlib import Path
      Path("/m/leak").read_text()
      """

      assert {:error, %ExMonty.Exception{type: :permission_error}} =
               ExMonty.Sandbox.run(code, mounts: mounts)
    end

    test "relative symlink that walks outside is rejected", %{tmp: tmp} do
      mount_root = Path.join(tmp, "mounted")
      File.mkdir_p!(mount_root)
      File.write!(Path.join(tmp, "outer.txt"), "outer secret")

      # ../outer.txt — a relative symlink pointing one level above the mount.
      :ok = File.ln_s("../outer.txt", Path.join(mount_root, "rel_leak"))

      mounts = Mount.new!() |> Mount.add!("/m", mount_root, :read_only)

      code = """
      from pathlib import Path
      Path("/m/rel_leak").read_text()
      """

      assert {:error, %ExMonty.Exception{type: :permission_error}} =
               ExMonty.Sandbox.run(code, mounts: mounts)
    end
  end

  describe "path traversal blocked" do
    test ".. segments cannot escape the mount root", %{tmp: tmp} do
      mount_root = Path.join(tmp, "mounted")
      File.mkdir_p!(mount_root)
      File.write!(Path.join(tmp, "secret.txt"), "outside")

      mounts = Mount.new!() |> Mount.add!("/m", mount_root, :read_only)

      # Sandbox tries `/m/../secret.txt` which would resolve to
      # mount_root/../secret.txt = tmp/secret.txt on the host.
      code = """
      from pathlib import Path
      Path("/m/../secret.txt").read_text()
      """

      # Either upstream resolves the path before checking (so it falls
      # outside any mount → PermissionError via on_no_handler) or the
      # path-security layer flags it. Either way, the read must fail.
      assert {:error, %ExMonty.Exception{type: type}} =
               ExMonty.Sandbox.run(code, mounts: mounts)

      assert type in [:permission_error, :file_not_found_error]
    end
  end

  describe "construction validation" do
    test "rejects relative virtual path" do
      m = Mount.new!()
      assert {:error, :invalid_virtual_path} = Mount.add(m, "data", "/tmp", :read_only)
    end

    test "rejects empty virtual path" do
      m = Mount.new!()
      assert {:error, :invalid_virtual_path} = Mount.add(m, "", "/tmp", :read_only)
    end

    test "rejects host path that's a regular file", %{tmp: tmp} do
      file = Path.join(tmp, "f.txt")
      File.write!(file, "")
      m = Mount.new!()

      assert {:error, reason} = Mount.add(m, "/x", file, :read_only)
      assert reason in [:host_path_not_directory, :invalid_virtual_path]
    end

    test "rejects nonexistent host path" do
      missing = "/tmp/ex_monty_does_not_exist_#{:erlang.unique_integer([:positive])}"
      m = Mount.new!()

      assert {:error, reason} = Mount.add(m, "/x", missing, :read_only)
      assert reason in [:host_path_not_found, :host_path_canonicalize_failed]
    end
  end
end
