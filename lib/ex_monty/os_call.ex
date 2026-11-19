defmodule ExMonty.OsCall do
  @moduledoc """
  Represents a paused OS/filesystem call during interactive Python execution.

  When Python code performs file I/O or OS operations (e.g., `Path.read_text()`),
  execution pauses and this struct is returned with the call details.

  The sandbox never reads the host clock or sleeps on its own: `time.time()`,
  `datetime.now()`, `time.sleep()` and `asyncio.sleep()` all arrive here, so
  the host decides what time it is and whether (and how) to wait. Resume a
  `:sleep` with `{:ok, nil}` once the delay has been honoured (or skipped).
  An `:async_sleep` may also be resumed with `:pending` and resolved later
  through `resume_futures/2`, letting sibling tasks run meanwhile.

  Resume with `:no_handler` to decline a call: the sandbox raises the
  canonical exception for it (`PermissionError` for filesystem calls,
  `ModuleNotFoundError` for `:import`, ...).

  ## Fields

    * `:function` - the OS function as an atom:
      * filesystem — `:exists`, `:is_file`, `:is_dir`, `:is_symlink`,
        `:read_text`, `:read_bytes`, `:write_text`, `:write_bytes`,
        `:append_text`, `:append_bytes`, `:open`, `:create_exclusive`,
        `:mkdir`, `:unlink`, `:rmdir`, `:iterdir`, `:scan`, `:stat`,
        `:rename`, `:resolve`, `:absolute`
      * environment — `:getenv`, `:get_environ`, `:urandom`
      * clock — `:date_today`, `:datetime_now`, `:time`, `:sleep`,
        `:async_sleep`
      * modules — `:import`, `:import_stubs`, `:get_type_hints`
    * `:args` - list of positional arguments. For `:open`, this is
      `[{:path, path}, mode]` where `mode` is a Python mode string (`"r"`,
      `"w"`, `"a"`, `"rb"`, ...). A handler servicing `:open` returns a
      `{:file_handle, %{path: path, mode: mode, position: pos}}` value.
      `:sleep` / `:async_sleep` carry `[seconds]`; `:import` carries
      `[module_name, search_path]`.
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
