defmodule ExMonty.MountLifecycleTest do
  @moduledoc """
  Cumulative limits, write-bytes enforcement, and concurrent / paused
  lease semantics.
  """
  use ExUnit.Case, async: false

  alias ExMonty.Mount

  setup do
    tmp =
      Path.join(System.tmp_dir!(), "ex_monty_mount_lc_#{:erlang.unique_integer([:positive])}")

    File.mkdir_p!(tmp)
    on_exit(fn -> File.rm_rf!(tmp) end)
    %{tmp: tmp}
  end

  describe "write_bytes_limit cumulative enforcement" do
    test "rejects when cumulative writes exceed the limit", %{tmp: tmp} do
      mounts =
        Mount.new!()
        |> Mount.add!("/o", tmp, :overlay, write_bytes_limit: 16)

      # First run: 10 bytes — allowed.
      assert {:ok, _, _} =
               ExMonty.Sandbox.run(
                 """
                 from pathlib import Path
                 Path("/o/a.txt").write_text("0123456789")
                 """,
                 mounts: mounts
               )

      # Second run on same mount: 10 more bytes → cumulative 20 > 16,
      # should error.
      assert {:error, %ExMonty.Exception{}} =
               ExMonty.Sandbox.run(
                 """
                 from pathlib import Path
                 Path("/o/b.txt").write_text("ABCDEFGHIJ")
                 """,
                 mounts: mounts
               )
    end

    test "fresh mount resets the cumulative counter", %{tmp: tmp} do
      m1 =
        Mount.new!()
        |> Mount.add!("/o", tmp, :overlay, write_bytes_limit: 16)

      ExMonty.Sandbox.run(
        """
        from pathlib import Path
        Path("/o/a.txt").write_text("0123456789")
        """,
        mounts: m1
      )

      # New mount: counter is 0 again, so a 10-byte write fits under 16.
      m2 =
        Mount.new!()
        |> Mount.add!("/o", tmp, :overlay, write_bytes_limit: 16)

      assert {:ok, _, _} =
               ExMonty.Sandbox.run(
                 """
                 from pathlib import Path
                 Path("/o/b.txt").write_text("ABCDEFGHIJ")
                 """,
                 mounts: m2
               )
    end
  end

  describe "interleave-during-pause" do
    test "second checkout fails while a run is paused at a function_call", %{tmp: tmp} do
      mounts = Mount.new!() |> Mount.add!("/data", tmp, :read_only)

      parent = self()

      slow_handler =
        fn _args, _kwargs ->
          send(parent, :paused)
          # Block until the parent unblocks us.
          receive do
            :continue -> {:ok, "result"}
          after
            5_000 -> {:error, :runtime_error, "test handler timeout"}
          end
        end

      runner =
        spawn_link(fn ->
          result =
            ExMonty.Sandbox.run(
              """
              compute()
              """,
              mounts: mounts,
              functions: %{"compute" => slow_handler}
            )

          send(parent, {:done, result})
        end)

      # Wait until the run is paused inside the handler.
      assert_receive :paused, 1_000

      # While paused, the source mount is leased — concurrent checkout fails.
      assert {:error, :mount_in_use} = Mount.checkout(mounts)

      # Let the handler finish, the run completes, lease is released.
      send(runner, :continue)
      assert_receive {:done, {:ok, "result", _}}, 1_000

      # Mount is usable again.
      assert {:ok, _} = Mount.checkout(mounts)
    end
  end

  describe "post-error reuse" do
    test "mount reusable after consecutive runs with mixed outcomes", %{tmp: tmp} do
      mounts = Mount.new!() |> Mount.add!("/data", tmp, :read_only)

      assert {:ok, _, _} = ExMonty.Sandbox.run("1 + 1", mounts: mounts)

      assert {:error, %ExMonty.Exception{type: :zero_division_error}} =
               ExMonty.Sandbox.run("1 / 0", mounts: mounts)

      assert {:ok, _, _} = ExMonty.Sandbox.run("2 + 2", mounts: mounts)

      assert {:ok, _, _} =
               ExMonty.Sandbox.run(
                 """
                 from pathlib import Path
                 Path("/data").exists()
                 """,
                 mounts: mounts
               )
    end
  end
end
