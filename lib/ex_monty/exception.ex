defmodule ExMonty.Exception do
  @moduledoc """
  Represents a Python exception raised during execution.

  ## Fields

    * `:type` - the exception type as an atom (e.g., `:value_error`, `:type_error`)
    * `:message` - the exception message string, or `nil`
    * `:traceback` - list of `ExMonty.StackFrame` structs
  """

  @type t :: %__MODULE__{
          type: atom(),
          message: String.t() | nil,
          traceback: [ExMonty.StackFrame.t()]
        }

  defstruct [:type, :message, traceback: []]
end

defmodule ExMonty.StackFrame do
  @moduledoc """
  Represents a single frame in a Python traceback.

  ## Fields

    * `:filename` - source file name
    * `:line` - start line number (1-based)
    * `:column` - start column number (1-based)
    * `:end_line` - end line number
    * `:end_column` - end column number
    * `:name` - function/frame name, or `nil` for module-level code
  """

  @type t :: %__MODULE__{
          filename: String.t(),
          line: non_neg_integer(),
          column: non_neg_integer(),
          end_line: non_neg_integer(),
          end_column: non_neg_integer(),
          name: String.t() | nil
        }

  defstruct [:filename, :line, :column, :end_line, :end_column, :name]
end
