defmodule ExMonty.Mount do
  @moduledoc """
  Host filesystem mounts for sandboxed Python execution.

  A mount maps a virtual path inside the sandbox (e.g. `/data`) to a real
  host directory (e.g. `/var/lib/myapp/data`) with a configurable access
  policy. Path canonicalisation, boundary checks, and symlink-escape
  detection are enforced by upstream monty regardless of mode.

  Mounts are stateful — overlay writes and `write_bytes_used` accumulate
  on the mount across runs. Discard accumulated state by constructing a
  fresh mount.

  ## Modes

    * `:read_only` — reads pass through to the host; writes raise
      `PermissionError`.
    * `:read_write` — reads and writes both hit the real disk. **Footgun:**
      sandbox code can modify real host files. Use with care.
    * `:overlay` — reads fall through to the host; writes are captured
      in-memory; the host directory is untouched.

  ## Examples

      # Read-only access to a data directory
      {:ok, mounts} = ExMonty.Mount.new()
      {:ok, mounts} = ExMonty.Mount.add(mounts, "/data", "/var/lib/myapp/data", :read_only)

      ExMonty.Sandbox.run(
        \"\"\"
        from pathlib import Path
        Path("/data/users.csv").read_text()
        \"\"\",
        mounts: mounts
      )

      # Pipe-friendly with `add!/5`
      mounts =
        ExMonty.Mount.new!()
        |> ExMonty.Mount.add!("/data",    "/var/lib/myapp/data",    :read_only)
        |> ExMonty.Mount.add!("/scratch", "/tmp/sandbox-scratch",   :overlay)
        |> ExMonty.Mount.add!("/output",  "/var/lib/myapp/output",  :read_write,
             write_bytes_limit: 10_000_000)

  ## Lease lifecycle

  `Sandbox.run` checks out a `Lease` for the duration of the run and
  releases it via `try/after`. Most users never touch `checkout/1` /
  `release/1` directly. While a lease is alive against a mount,
  `add/5` and concurrent `checkout/1` return `{:error, :mount_in_use}`.
  `count/1` and `list/1` keep working — they read from a side-channel
  that survives a lease.
  """

  alias ExMonty.Native

  @enforce_keys [:ref]
  defstruct [:ref]

  @opaque t :: %__MODULE__{ref: reference()}

  @type mode :: :read_only | :read_write | :overlay

  @type add_opts :: [write_bytes_limit: pos_integer()]

  @type add_error ::
          :invalid_virtual_path
          | :invalid_mode
          | :host_path_not_found
          | :host_path_not_directory
          | :host_path_canonicalize_failed
          | :mount_in_use
          | {:already_mounted, virtual :: String.t()}

  @type list_entry :: %{
          virtual: String.t(),
          host: String.t(),
          mode: mode(),
          write_bytes_limit: pos_integer() | :unlimited
        }

  defmodule Lease do
    @moduledoc """
    Opaque per-run lease taken from a `ExMonty.Mount`. Released via
    `ExMonty.Mount.release/1` (idempotent).
    """

    @enforce_keys [:ref]
    defstruct [:ref]

    @opaque t :: %__MODULE__{ref: reference()}
  end

  # ── Construction ────────────────────────────────────────────────────────

  @doc "Creates an empty mount table."
  @spec new() :: {:ok, t()}
  def new, do: {:ok, %__MODULE__{ref: Native.mounts_new()}}

  @doc "Bang version of `new/0`."
  @spec new!() :: t()
  def new!, do: %__MODULE__{ref: Native.mounts_new()}

  @doc """
  Adds a mount point. Returns `{:ok, mount}` on success.

  See module doc for mode semantics. `add_opts` accepts:

    * `:write_bytes_limit` — cumulative cap (in bytes) on writes against
      this mount across all runs. Counter does not reset between runs;
      construct a fresh mount to reset.
  """
  @spec add(t(), String.t(), String.t(), mode(), add_opts()) ::
          {:ok, t()} | {:error, add_error()}
  def add(%__MODULE__{ref: ref} = mount, virtual_path, host_path, mode, opts \\ [])
      when is_binary(virtual_path) and is_binary(host_path) and is_atom(mode) and is_list(opts) do
    write_bytes_limit = Keyword.get(opts, :write_bytes_limit)

    case Native.mounts_add(ref, virtual_path, host_path, mode, write_bytes_limit) do
      :ok -> {:ok, mount}
      {:error, _reason} = err -> err
    end
  end

  @doc "Bang version of `add/5`. Raises on error."
  @spec add!(t(), String.t(), String.t(), mode(), add_opts()) :: t()
  def add!(mount, virtual_path, host_path, mode, opts \\ []) do
    case add(mount, virtual_path, host_path, mode, opts) do
      {:ok, m} -> m
      {:error, reason} -> raise ArgumentError, "Mount.add!/5 failed: #{inspect(reason)}"
    end
  end

  # ── Inspection ──────────────────────────────────────────────────────────

  @doc """
  Returns the number of configured mounts.

  Permissive under lease — reads from the descriptor side-channel without
  contending with an active run.
  """
  @spec count(t()) :: non_neg_integer()
  def count(%__MODULE__{ref: ref}), do: Native.mounts_count(ref)

  @doc """
  Returns the configured mounts as a list of maps.

  Permissive under lease (v1 returns descriptor-only state — no live
  `write_bytes_used` until upstream exposes a public getter).
  """
  @spec list(t()) :: [list_entry()]
  def list(%__MODULE__{ref: ref}), do: Native.mounts_list(ref)

  # ── Lease lifecycle (advanced) ──────────────────────────────────────────

  @doc """
  Takes a lease for the duration of a single sandbox run. While the lease
  is alive, `add/5` against this mount returns `{:error, :mount_in_use}`.

  Most users don't call this directly — `Sandbox.run` handles it.
  """
  @spec checkout(t()) :: {:ok, Lease.t()} | {:error, :mount_in_use}
  def checkout(%__MODULE__{ref: ref}) do
    case Native.mounts_checkout(ref) do
      {:ok, lease_ref} -> {:ok, %Lease{ref: lease_ref}}
      {:error, :mount_in_use} = err -> err
    end
  end

  @doc """
  Releases a lease back to its source mount. Idempotent — calling on an
  already-released lease returns `:ok`.
  """
  @spec release(Lease.t()) :: :ok
  def release(%Lease{ref: ref}) do
    Native.mounts_release(ref)
    :ok
  end
end

defmodule ExMonty.MountInUseError do
  @moduledoc """
  Raised by `ExMonty.Mount.list!/1` (and other bang variants, future) when
  a mount is currently leased to a run.
  """
  defexception [:message]

  @impl true
  def exception(opts) do
    msg = Keyword.get(opts, :message, "mount is currently leased to an active run")
    %__MODULE__{message: msg}
  end
end
