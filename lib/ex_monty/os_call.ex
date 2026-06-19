defmodule ExMonty.OsCall do
  @moduledoc """
  Represents a paused OS/filesystem call during interactive Python execution.

  When Python code performs file I/O or OS operations (e.g., `Path.read_text()`),
  execution pauses and this struct is returned with the call details.

  ## Fields

    * `:function` - the OS function as an atom (e.g., `:read_text`, `:exists`,
      `:write_text`, `:append_text`, `:open`, `:getenv`, `:get_environ`,
      `:date_today`, `:datetime_now`)
    * `:args` - list of positional arguments. For `:open`, this is
      `[{:path, path}, mode]` where `mode` is a Python mode string (`"r"`,
      `"w"`, `"a"`, `"rb"`, ...). A handler servicing `:open` returns a
      `{:file_handle, %{path: path, mode: mode, position: pos}}` value.
    * `:kwargs` - map of keyword arguments
    * `:kwargs` - map of keyword arguments
    * `:call_id` - unique identifier for this call within the execution
  """

  @type t :: %__MODULE__{
          function: atom(),
          args: list(),
          kwargs: map(),
          call_id: non_neg_integer()
        }

  defstruct [:function, :args, :kwargs, :call_id]
end
