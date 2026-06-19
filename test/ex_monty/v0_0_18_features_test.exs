defmodule ExMonty.V0018FeaturesTest do
  @moduledoc """
  Tests for capabilities added in monty v0.0.18.

  The headline change is the buffered `open()` builtin (with context-manager
  support) and the associated file-handle object. Upstream also refactored the
  OS-call surface from a bare `OsFunction` tag into a typed `OsFunctionCall`
  carrying its args — these tests lock in the ExMonty-facing behaviour that
  refactor produces: the new `:open`, `:append_text`, and `:append_bytes` os
  calls, and the `{:file_handle, ...}` object encoding.
  """
  use ExUnit.Case, async: false

  alias ExMonty.Mount

  setup do
    tmp =
      Path.join(System.tmp_dir!(), "ex_monty_v18_#{:erlang.unique_integer([:positive])}")

    File.mkdir_p!(tmp)
    on_exit(fn -> File.rm_rf!(tmp) end)
    %{tmp: tmp}
  end

  describe "open() through a mount" do
    test "write mode creates the file and hits the host", %{tmp: tmp} do
      mounts = Mount.new!() |> Mount.add!("/rw", tmp, :read_write)

      code = """
      with open("/rw/out.txt", "w") as f:
          f.write("hello via open")
      open("/rw/out.txt").read()
      """

      assert {:ok, "hello via open", ""} = ExMonty.Sandbox.run(code, mounts: mounts)
      assert File.read!(Path.join(tmp, "out.txt")) == "hello via open"
    end

    test "append mode preserves existing content", %{tmp: tmp} do
      File.write!(Path.join(tmp, "log.txt"), "line1\n")
      mounts = Mount.new!() |> Mount.add!("/rw", tmp, :read_write)

      code = """
      with open("/rw/log.txt", "a") as f:
          f.write("line2\\n")
      open("/rw/log.txt").read()
      """

      assert {:ok, "line1\nline2\n", ""} = ExMonty.Sandbox.run(code, mounts: mounts)
    end

    test "read mode of a missing file raises", %{tmp: tmp} do
      mounts = Mount.new!() |> Mount.add!("/rw", tmp, :read_write)

      code = """
      open("/rw/nope.txt").read()
      """

      assert {:error, %ExMonty.Exception{}} = ExMonty.Sandbox.run(code, mounts: mounts)
    end
  end

  describe ":open os call surfaced to a handler" do
    test "surfaces the path and mode, accepts a file_handle result" do
      os = %{
        open: fn [{:path, path}, mode], _kwargs ->
          {:ok, {:file_handle, %{path: path, mode: mode, position: 0}}}
        end
      }

      code = """
      f = open("/x", "r")
      type(f).__name__
      """

      assert {:ok, "_io.TextIOWrapper", ""} = ExMonty.Sandbox.run(code, os: os)
    end

    test "binary mode round-trips through the file_handle encoding" do
      os = %{
        open: fn [{:path, path}, mode], _kwargs ->
          {:ok, {:file_handle, %{path: path, mode: mode, position: 0}}}
        end
      }

      code = """
      f = open("/x", "rb")
      type(f).__name__
      """

      assert {:ok, "_io.BufferedReader", ""} = ExMonty.Sandbox.run(code, os: os)
    end
  end
end
