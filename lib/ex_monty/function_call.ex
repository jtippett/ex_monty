defmodule ExMonty.FunctionCall do
  @moduledoc """
  Represents a paused external function call or dataclass method call during
  interactive Python execution.

  When Python code calls an external function (one declared in `external_functions`),
  execution pauses and this struct is returned with the call details under the
  `{:function_call, call, snapshot, output}` tag.

  When Python code calls a method on a dataclass instance, execution pauses and
  this struct is returned under the `{:method_call, call, snapshot, output}` tag.
  For method calls, the first element of `:args` is the dataclass instance (`self`).

  ## Fields

    * `:name` - the function or method name as a string
    * `:args` - list of positional arguments (for method calls, first arg is `self`)
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
