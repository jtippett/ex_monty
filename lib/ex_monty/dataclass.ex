defmodule ExMonty.Dataclass do
  @moduledoc """
  Represents a Python dataclass instance.

  ## Fields

    * `:name` - the dataclass type name
    * `:fields` - map of field name strings to values
    * `:frozen` - whether the dataclass is frozen (immutable)
  """

  @type t :: %__MODULE__{
          name: String.t(),
          fields: map(),
          frozen: boolean()
        }

  defstruct [:name, :fields, frozen: false]
end
