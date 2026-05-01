defmodule ExMonty.Native do
  @moduledoc false

  @version Mix.Project.config()[:version]

  use RustlerPrecompiled,
    otp_app: :ex_monty,
    crate: "ex_monty",
    base_url: "https://github.com/jtippett/ex_monty/releases/download/v#{@version}",
    version: @version,
    targets: ~w(
      aarch64-apple-darwin
      x86_64-apple-darwin
      x86_64-unknown-linux-gnu
      aarch64-unknown-linux-gnu
    ),
    force_build: System.get_env("EXMONTY_BUILD") in ["1", "true"]

  # Core
  def compile(_code, _script_name, _input_names),
    do: :erlang.nif_error(:nif_not_loaded)

  def run(_runner, _inputs, _limits), do: :erlang.nif_error(:nif_not_loaded)
  def run_no_limits(_runner, _inputs), do: :erlang.nif_error(:nif_not_loaded)

  # Interactive
  def start(_runner, _inputs, _limits), do: :erlang.nif_error(:nif_not_loaded)
  def resume(_snapshot, _result), do: :erlang.nif_error(:nif_not_loaded)
  def resume_futures(_futures, _results), do: :erlang.nif_error(:nif_not_loaded)
  def pending_call_ids(_futures), do: :erlang.nif_error(:nif_not_loaded)

  # Serialization
  def dump_runner(_runner), do: :erlang.nif_error(:nif_not_loaded)
  def load_runner(_binary), do: :erlang.nif_error(:nif_not_loaded)
  def dump_snapshot(_snapshot), do: :erlang.nif_error(:nif_not_loaded)
  def load_snapshot(_binary), do: :erlang.nif_error(:nif_not_loaded)
  def dump_future_snapshot(_futures), do: :erlang.nif_error(:nif_not_loaded)
  def load_future_snapshot(_binary), do: :erlang.nif_error(:nif_not_loaded)

  # Mounts
  def mounts_new, do: :erlang.nif_error(:nif_not_loaded)
  def mounts_count(_ref), do: :erlang.nif_error(:nif_not_loaded)
  def mounts_list(_ref), do: :erlang.nif_error(:nif_not_loaded)

  def mounts_add(_ref, _virtual, _host, _mode, _write_bytes_limit),
    do: :erlang.nif_error(:nif_not_loaded)

  def mounts_checkout(_ref), do: :erlang.nif_error(:nif_not_loaded)
  def mounts_release(_lease_ref), do: :erlang.nif_error(:nif_not_loaded)
end
