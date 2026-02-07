defmodule ExMonty.Native do
  @moduledoc false

  use Rustler,
    otp_app: :ex_monty,
    crate: "ex_monty"

  # Core
  def compile(_code, _script_name, _input_names, _external_fns),
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
end
