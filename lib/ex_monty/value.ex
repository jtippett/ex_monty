defmodule ExMonty.Value do
  @moduledoc """
  Conversions for values produced by the Monty runtime.

  Monty output is a faithful mapping of Python values (see the Type Mapping
  table in the README): sets become `MapSet`s, tuples stay tuples, bytes are
  tagged `{:bytes, binary}`, and so on. Faithful is right for Elixir callers,
  but none of those shapes are JSON-encodable, so hosts that serialize results
  (API responses, persisted tool-call records) need a lossy projection.
  """

  @doc """
  Project a Monty output value onto JSON-safe terms.

  The result contains only `nil`, booleans, numbers, UTF-8 strings, lists, and
  maps with string keys — encodable by any JSON library. The projection is
  lossy but legible, following `json.dumps` conventions where Python has them:

    * sets and frozensets → sorted lists; tuples and named tuples → lists/maps
    * `{:bytes, b}` → the string itself when valid UTF-8, else Base64
    * non-finite floats → `"NaN"` / `"Infinity"` / `"-Infinity"`
    * dates and datetimes → ISO 8601 strings; timedeltas/timezones → maps
    * dataclasses → their field map; `{:repr, r}` and Ellipsis → repr strings
    * non-string dict keys → `"null"` / `"true"` / `"1"` etc.
  """
  def to_json_safe(value) when is_number(value) or is_binary(value), do: value
  def to_json_safe(value) when is_nil(value) or is_boolean(value), do: value
  def to_json_safe(:nan), do: "NaN"
  def to_json_safe(:infinity), do: "Infinity"
  def to_json_safe(:neg_infinity), do: "-Infinity"
  def to_json_safe(:ellipsis), do: "Ellipsis"
  def to_json_safe(value) when is_atom(value), do: Atom.to_string(value)

  def to_json_safe({:bytes, bin}) do
    if String.valid?(bin), do: bin, else: Base.encode64(bin)
  end

  def to_json_safe({:path, path}), do: path
  def to_json_safe({:repr, repr}), do: repr

  def to_json_safe({:date, %{year: y, month: m, day: d}}) do
    Date.new!(y, m, d) |> Date.to_iso8601()
  end

  def to_json_safe({:datetime, %{} = dt} = _value) do
    %{year: y, month: mo, day: d, hour: h, minute: mi, second: s, microsecond: us} = dt
    precision = if us == 0, do: 0, else: 6
    naive = NaiveDateTime.new!(y, mo, d, h, mi, s, {us, precision})
    NaiveDateTime.to_iso8601(naive) <> offset_suffix(dt.offset_seconds)
  end

  def to_json_safe({:timedelta, %{days: d, seconds: s, microseconds: us}}) do
    %{"days" => d, "seconds" => s, "microseconds" => us}
  end

  def to_json_safe({:timezone, %{offset_seconds: offset, name: name}}) do
    %{"offset_seconds" => offset, "name" => name}
  end

  def to_json_safe({:file_handle, %{path: path, mode: mode, position: position}}) do
    %{"path" => path, "mode" => mode, "position" => position}
  end

  def to_json_safe({:named_tuple, _type_name, fields}) do
    Map.new(fields, fn {name, value} -> {name, to_json_safe(value)} end)
  end

  def to_json_safe(%ExMonty.Dataclass{fields: fields}) do
    Map.new(fields, fn {name, value} -> {name, to_json_safe(value)} end)
  end

  def to_json_safe(%MapSet{} = set) do
    set |> Enum.map(&to_json_safe/1) |> Enum.sort()
  end

  def to_json_safe(list) when is_list(list), do: Enum.map(list, &to_json_safe/1)

  def to_json_safe(tuple) when is_tuple(tuple) do
    tuple |> Tuple.to_list() |> Enum.map(&to_json_safe/1)
  end

  def to_json_safe(map) when is_map(map) do
    Map.new(map, fn {key, value} -> {json_key(key), to_json_safe(value)} end)
  end

  defp json_key(key) when is_binary(key), do: key
  defp json_key(nil), do: "null"
  defp json_key(true), do: "true"
  defp json_key(false), do: "false"
  defp json_key(key) when is_number(key), do: to_string(key)

  defp json_key(key) do
    case to_json_safe(key) do
      string when is_binary(string) -> string
      other -> inspect(other)
    end
  end

  defp offset_suffix(nil), do: ""

  defp offset_suffix(offset_seconds) do
    sign = if offset_seconds < 0, do: "-", else: "+"
    total = abs(offset_seconds)
    hours = total |> div(3600) |> Integer.to_string() |> String.pad_leading(2, "0")
    minutes = total |> rem(3600) |> div(60) |> Integer.to_string() |> String.pad_leading(2, "0")
    "#{sign}#{hours}:#{minutes}"
  end
end
