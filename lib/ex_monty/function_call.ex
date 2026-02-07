defmodule ExMonty.FunctionCall do
  @moduledoc """
  Represents a paused external function call during interactive Python execution.

  When Python code calls an external function (one declared in `external_functions`),
  execution pauses and this struct is returned with the call details.

  ## Fields

    * `:name` - the function name as a string
    * `:args` - list of positional arguments
    * `:kwargs` - map of keyword arguments
    * `:call_id` - unique identifier for this call within the execution
  """

  @type t :: %__MODULE__{
          name: String.t(),
          args: list(),
          kwargs: map(),
          call_id: non_neg_integer()
        }

  defstruct [:name, :args, :kwargs, :call_id]
end
