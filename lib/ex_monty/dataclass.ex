defmodule ExMonty.Dataclass do
  @moduledoc """
  Represents a Python dataclass instance.

  ## Fields

    * `:name` - the dataclass type name
    * `:fields` - map of field name strings to values
    * `:field_names` - declaration-order field names (derived from `fields` keys when `nil`)
    * `:type_id` - monty type identity (encoded as `0` when `nil`)
    * `:frozen` - whether the dataclass is frozen (immutable)
  """

  @type t :: %__MODULE__{
          name: String.t(),
          fields: map(),
          field_names: [String.t()] | nil,
          type_id: non_neg_integer() | nil,
          frozen: boolean()
        }

  defstruct [:name, :fields, :field_names, :type_id, frozen: false]
end
