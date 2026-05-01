defmodule ExMonty.MountTest do
  use ExUnit.Case, async: false

  alias ExMonty.Mount

  setup do
    tmp =
      Path.join(System.tmp_dir!(), "ex_monty_mount_test_#{:erlang.unique_integer([:positive])}")

    File.mkdir_p!(tmp)
    on_exit(fn -> File.rm_rf!(tmp) end)
    %{tmp: tmp}
  end

  describe "new/0 and new!/0" do
    test "new/0 returns an empty mount" do
      assert {:ok, %Mount{}} = Mount.new()
    end

    test "new!/0 returns an empty mount struct" do
      assert %Mount{} = Mount.new!()
    end

    test "new mount has zero count and empty list" do
      m = Mount.new!()
      assert Mount.count(m) == 0
      assert Mount.list(m) == []
    end
  end

  describe "add/5 success" do
    test "adds a read-only mount", %{tmp: tmp} do
      assert {:ok, m} = Mount.new()
      assert {:ok, ^m} = Mount.add(m, "/data", tmp, :read_only)
      assert Mount.count(m) == 1

      assert [%{virtual: "/data", host: ^tmp, mode: :read_only, write_bytes_limit: :unlimited}] =
               Mount.list(m)
    end

    test "adds a read-write mount", %{tmp: tmp} do
      assert {:ok, m} = Mount.new()
      assert {:ok, _} = Mount.add(m, "/rw", tmp, :read_write)
      assert [%{mode: :read_write}] = Mount.list(m)
    end

    test "adds an overlay mount", %{tmp: tmp} do
      assert {:ok, m} = Mount.new()
      assert {:ok, _} = Mount.add(m, "/o", tmp, :overlay)
      assert [%{mode: :overlay}] = Mount.list(m)
    end

    test "preserves write_bytes_limit in list/1", %{tmp: tmp} do
      m = Mount.new!() |> Mount.add!("/o", tmp, :overlay, write_bytes_limit: 1024)
      assert [%{write_bytes_limit: 1024}] = Mount.list(m)
    end

    test "list/1 returns :unlimited when no limit set", %{tmp: tmp} do
      m = Mount.new!() |> Mount.add!("/o", tmp, :overlay)
      assert [%{write_bytes_limit: :unlimited}] = Mount.list(m)
    end

    test "multiple mounts in add order", %{tmp: tmp} do
      a = Path.join(tmp, "a")
      b = Path.join(tmp, "b")
      File.mkdir_p!(a)
      File.mkdir_p!(b)

      m =
        Mount.new!()
        |> Mount.add!("/a", a, :read_only)
        |> Mount.add!("/b", b, :overlay)

      assert Mount.count(m) == 2
      assert [%{virtual: "/a"}, %{virtual: "/b"}] = Mount.list(m)
    end
  end

  describe "add/5 errors" do
    test "rejects relative virtual path", %{tmp: tmp} do
      m = Mount.new!()
      assert {:error, :invalid_virtual_path} = Mount.add(m, "data", tmp, :read_only)
    end

    test "rejects nonexistent host path" do
      m = Mount.new!()
      missing = "/tmp/ex_monty_does_not_exist_#{:erlang.unique_integer([:positive])}"
      assert {:error, reason} = Mount.add(m, "/x", missing, :read_only)
      assert reason in [:host_path_not_found, :host_path_canonicalize_failed]
    end

    test "rejects host path that's a file, not a directory", %{tmp: tmp} do
      file = Path.join(tmp, "regular.txt")
      File.write!(file, "")
      m = Mount.new!()
      assert {:error, reason} = Mount.add(m, "/x", file, :read_only)
      assert reason in [:host_path_not_directory, :invalid_virtual_path]
    end

    test "rejects unknown mode atom", %{tmp: tmp} do
      m = Mount.new!()
      assert {:error, :invalid_mode} = Mount.add(m, "/x", tmp, :bogus_mode)
    end

    test "rejects duplicate virtual path", %{tmp: tmp} do
      m = Mount.new!() |> Mount.add!("/data", tmp, :read_only)
      assert {:error, {:already_mounted, "/data"}} = Mount.add(m, "/data", tmp, :read_only)
    end
  end

  describe "add!/5" do
    test "raises ArgumentError on error", %{tmp: tmp} do
      m = Mount.new!()

      assert_raise ArgumentError, ~r/invalid_mode/, fn ->
        Mount.add!(m, "/x", tmp, :bogus_mode)
      end
    end

    test "returns mount on success", %{tmp: tmp} do
      assert %Mount{} = Mount.new!() |> Mount.add!("/data", tmp, :read_only)
    end
  end

  describe "checkout/1 and release/1 lifecycle" do
    test "checkout returns a lease", %{tmp: tmp} do
      m = Mount.new!() |> Mount.add!("/data", tmp, :read_only)
      assert {:ok, %Mount.Lease{}} = Mount.checkout(m)
    end

    test "second concurrent checkout errors", %{tmp: tmp} do
      m = Mount.new!() |> Mount.add!("/data", tmp, :read_only)
      assert {:ok, _lease} = Mount.checkout(m)
      assert {:error, :mount_in_use} = Mount.checkout(m)
    end

    test "release returns :ok and lets next checkout succeed", %{tmp: tmp} do
      m = Mount.new!() |> Mount.add!("/data", tmp, :read_only)
      {:ok, lease} = Mount.checkout(m)
      assert :ok = Mount.release(lease)
      assert {:ok, _} = Mount.checkout(m)
    end

    test "release is idempotent", %{tmp: tmp} do
      m = Mount.new!() |> Mount.add!("/data", tmp, :read_only)
      {:ok, lease} = Mount.checkout(m)
      assert :ok = Mount.release(lease)
      assert :ok = Mount.release(lease)
      assert :ok = Mount.release(lease)
      # Mount still usable after triple-release
      assert {:ok, _} = Mount.checkout(m)
    end

    test "add/5 errors :mount_in_use while leased", %{tmp: tmp} do
      m = Mount.new!() |> Mount.add!("/data", tmp, :read_only)
      {:ok, _lease} = Mount.checkout(m)
      assert {:error, :mount_in_use} = Mount.add(m, "/x", tmp, :read_only)
    end

    test "add/5 succeeds again after release", %{tmp: tmp} do
      m = Mount.new!() |> Mount.add!("/data", tmp, :read_only)
      {:ok, lease} = Mount.checkout(m)
      Mount.release(lease)
      assert {:ok, _} = Mount.add(m, "/x", tmp, :read_only)
      assert Mount.count(m) == 2
    end
  end

  describe "permissive count/list under lease" do
    test "count/1 works while leased", %{tmp: tmp} do
      m =
        Mount.new!()
        |> Mount.add!("/a", tmp, :read_only)
        |> Mount.add!("/b", tmp, :overlay)

      assert Mount.count(m) == 2

      {:ok, lease} = Mount.checkout(m)
      assert Mount.count(m) == 2

      Mount.release(lease)
      assert Mount.count(m) == 2
    end

    test "list/1 works while leased", %{tmp: tmp} do
      m = Mount.new!() |> Mount.add!("/data", tmp, :read_only)
      {:ok, _lease} = Mount.checkout(m)
      assert [%{virtual: "/data", mode: :read_only}] = Mount.list(m)
    end
  end

  describe "concurrent release across processes" do
    test "two processes both calling release see :ok", %{tmp: tmp} do
      m = Mount.new!() |> Mount.add!("/data", tmp, :read_only)
      {:ok, lease} = Mount.checkout(m)

      parent = self()

      pid1 = spawn_link(fn -> send(parent, {:result, 1, Mount.release(lease)}) end)
      pid2 = spawn_link(fn -> send(parent, {:result, 2, Mount.release(lease)}) end)

      assert_receive {:result, 1, :ok}, 1_000
      assert_receive {:result, 2, :ok}, 1_000

      _ = pid1
      _ = pid2
      # Mount is usable after concurrent release
      assert {:ok, _} = Mount.checkout(m)
    end
  end

  describe "Drop fallback when lease GC'd" do
    test "abandoning a lease without release eventually frees the mount", %{tmp: tmp} do
      m = Mount.new!() |> Mount.add!("/data", tmp, :read_only)

      # Take a lease in an isolated scope so we can drop our reference.
      {:ok, _lease} = Mount.checkout(m)

      # Confirm it's leased.
      assert {:error, :mount_in_use} = Mount.checkout(m)

      # Force GC. The lease ResourceArc has no remaining BEAM references,
      # so it should be reclaimed and Drop runs, putting the table back.
      :erlang.garbage_collect()

      # On some BEAM scheduler timings this isn't instant; retry briefly.
      assert eventually(fn -> Mount.checkout(m) end, 1_000) == :ok
    end
  end

  defp eventually(fun, timeout_ms) do
    deadline = System.monotonic_time(:millisecond) + timeout_ms
    do_eventually(fun, deadline)
  end

  defp do_eventually(fun, deadline) do
    case fun.() do
      {:ok, _} ->
        :ok

      _ ->
        if System.monotonic_time(:millisecond) >= deadline do
          :timeout
        else
          :erlang.garbage_collect()
          Process.sleep(20)
          do_eventually(fun, deadline)
        end
    end
  end
end
