defmodule ExMonty.PseudoFS do
  @moduledoc """
  An in-memory virtual filesystem for sandboxed Python execution.

  Python code running in monty can use `pathlib.Path` and `os` module functions,
  which yield `:os_call` progress events. `PseudoFS` provides a complete handler
  that responds to these calls using an in-memory filesystem, so Python code can
  read and write files without touching the real filesystem.

  ## Usage

      fs = ExMonty.PseudoFS.new()
        |> ExMonty.PseudoFS.put_file("/data/config.json", ~s({"key": "value"}))
        |> ExMonty.PseudoFS.put_file("/data/readme.txt", "Hello world")
        |> ExMonty.PseudoFS.mkdir("/data/output")
        |> ExMonty.PseudoFS.put_env("API_KEY", "sk-secret123")

      {:ok, result, output} = ExMonty.Sandbox.run(code, os: fs)

  ## With ExMonty.Sandbox

  Pass the `PseudoFS` directly as the `:os` option — it implements the handler
  protocol expected by `ExMonty.Sandbox`:

      {:ok, result, output} = ExMonty.Sandbox.run(
        "from pathlib import Path; Path('/data/config.json').read_text()",
        os: ExMonty.PseudoFS.new()
          |> ExMonty.PseudoFS.put_file("/data/config.json", "content")
      )

  ## Supported Operations

  | Python                    | OS Function   | Notes                          |
  |---------------------------|---------------|--------------------------------|
  | `Path.exists()`           | `:exists`     | Checks files and directories   |
  | `Path.is_file()`          | `:is_file`    |                                |
  | `Path.is_dir()`           | `:is_dir`     |                                |
  | `Path.is_symlink()`       | `:is_symlink` | Always `False`                 |
  | `Path.read_text()`        | `:read_text`  |                                |
  | `Path.read_bytes()`       | `:read_bytes` |                                |
  | `Path.write_text(data)`   | `:write_text` | Creates parent dirs            |
  | `Path.write_bytes(data)`  | `:write_bytes`| Creates parent dirs            |
  | `Path.mkdir()`            | `:mkdir`      | Supports `parents`, `exist_ok` |
  | `Path.unlink()`           | `:unlink`     |                                |
  | `Path.rmdir()`            | `:rmdir`      | Must be empty                  |
  | `Path.iterdir()`          | `:iterdir`    | Lists direct children          |
  | `Path.stat()`             | `:stat`       | Returns stat_result tuple      |
  | `Path.rename(target)`     | `:rename`     |                                |
  | `Path.resolve()`          | `:resolve`    | Returns path as-is             |
  | `Path.absolute()`         | `:absolute`   | Returns path as-is             |
  | `os.getenv(key)`          | `:getenv`     |                                |
  | `os.environ`              | `:get_environ`|                                |
  """

  @type t :: %__MODULE__{
          files: %{String.t() => {binary(), non_neg_integer()}},
          dirs: MapSet.t(),
          env: %{String.t() => String.t()},
          mtime: float()
        }

  defstruct files: %{}, dirs: MapSet.new(), env: %{}, mtime: 1_700_000_000.0

  @doc """
  Creates a new empty pseudo filesystem.

  ## Options

    * `:mtime` - default modification time as Unix timestamp (default: `1_700_000_000.0`)
  """
  @spec new(keyword()) :: t()
  def new(opts \\ []) do
    %__MODULE__{mtime: Keyword.get(opts, :mtime, 1_700_000_000.0)}
  end

  @doc """
  Adds a file with text content.

  Parent directories are created automatically.

      fs = PseudoFS.new()
        |> PseudoFS.put_file("/etc/config.toml", "[settings]\\nkey = \\"value\\"")

  ## Options

    * `:mode` - file permission bits (default: `0o644`)
  """
  @spec put_file(t(), String.t(), binary(), keyword()) :: t()
  def put_file(%__MODULE__{} = fs, path, content, opts \\ []) when is_binary(path) do
    mode = Keyword.get(opts, :mode, 0o644)
    content = if is_binary(content), do: content, else: to_string(content)
    fs = ensure_parent_dirs(fs, path)
    %{fs | files: Map.put(fs.files, path, {content, mode})}
  end

  @doc """
  Adds a file with binary content.

  Parent directories are created automatically.
  """
  @spec put_bytes(t(), String.t(), binary(), keyword()) :: t()
  def put_bytes(%__MODULE__{} = fs, path, content, opts \\ []) when is_binary(path) do
    mode = Keyword.get(opts, :mode, 0o644)
    fs = ensure_parent_dirs(fs, path)
    %{fs | files: Map.put(fs.files, path, {content, mode})}
  end

  @doc """
  Creates a directory.

  Parent directories are created automatically.
  """
  @spec mkdir(t(), String.t()) :: t()
  def mkdir(%__MODULE__{} = fs, path) when is_binary(path) do
    fs = ensure_parent_dirs(fs, path)
    %{fs | dirs: MapSet.put(fs.dirs, path)}
  end

  @doc """
  Sets an environment variable.

      fs = PseudoFS.new() |> PseudoFS.put_env("API_KEY", "secret")
  """
  @spec put_env(t(), String.t(), String.t()) :: t()
  def put_env(%__MODULE__{} = fs, key, value) when is_binary(key) and is_binary(value) do
    %{fs | env: Map.put(fs.env, key, value)}
  end

  @doc """
  Sets multiple environment variables from a map.
  """
  @spec put_envs(t(), %{String.t() => String.t()}) :: t()
  def put_envs(%__MODULE__{} = fs, envs) when is_map(envs) do
    %{fs | env: Map.merge(fs.env, envs)}
  end

  @doc """
  Handles an OS call against this pseudo filesystem.

  This is the dispatch function used by `ExMonty.Sandbox`. You can also call
  it directly when driving the interactive loop manually.

  Returns `{:ok, value}` on success or `{:error, exc_type, message}` on failure.
  """
  @spec handle_os(t(), atom(), list(), map()) ::
          {:ok, term()} | {:error, atom(), String.t()} | {t(), {:ok, term()}} |
          {t(), {:error, atom(), String.t()}}
  def handle_os(%__MODULE__{} = fs, function, args, kwargs \\ %{}) do
    case dispatch(fs, function, args, kwargs) do
      {%__MODULE__{} = new_fs, result} -> {new_fs, result}
      result -> result
    end
  end

  # ── Dispatch ──────────────────────────────────────────────────────────────

  defp dispatch(fs, :get_environ, _args, _kwargs) do
    {:ok, fs.env}
  end

  defp dispatch(fs, :getenv, args, _kwargs) do
    [key | rest] = args
    default = List.first(rest)

    case Map.fetch(fs.env, key) do
      {:ok, value} -> {:ok, value}
      :error -> {:ok, default}
    end
  end

  defp dispatch(fs, :exists, [path | _], _kwargs) do
    path = extract_path(path)
    {:ok, Map.has_key?(fs.files, path) or MapSet.member?(fs.dirs, path)}
  end

  defp dispatch(fs, :is_file, [path | _], _kwargs) do
    path = extract_path(path)
    {:ok, Map.has_key?(fs.files, path)}
  end

  defp dispatch(fs, :is_dir, [path | _], _kwargs) do
    path = extract_path(path)
    {:ok, MapSet.member?(fs.dirs, path)}
  end

  defp dispatch(_fs, :is_symlink, _args, _kwargs) do
    {:ok, false}
  end

  defp dispatch(fs, :read_text, [path | _], _kwargs) do
    path = extract_path(path)

    case Map.fetch(fs.files, path) do
      {:ok, {content, _mode}} -> {:ok, content}
      :error -> file_not_found(path)
    end
  end

  defp dispatch(fs, :read_bytes, [path | _], _kwargs) do
    path = extract_path(path)

    case Map.fetch(fs.files, path) do
      {:ok, {content, _mode}} -> {:ok, {:bytes, content}}
      :error -> file_not_found(path)
    end
  end

  defp dispatch(fs, :write_text, [path, content | _], _kwargs) do
    path = extract_path(path)
    text = to_string(content)
    new_fs = ensure_parent_dirs(fs, path)
    new_fs = %{new_fs | files: Map.put(new_fs.files, path, {text, 0o644})}
    {new_fs, {:ok, byte_size(text)}}
  end

  defp dispatch(fs, :write_bytes, [path, {:bytes, content} | _], _kwargs) do
    path = extract_path(path)
    new_fs = ensure_parent_dirs(fs, path)
    new_fs = %{new_fs | files: Map.put(new_fs.files, path, {content, 0o644})}
    {new_fs, {:ok, byte_size(content)}}
  end

  defp dispatch(fs, :write_bytes, [path, content | _], _kwargs) when is_binary(content) do
    path = extract_path(path)
    new_fs = ensure_parent_dirs(fs, path)
    new_fs = %{new_fs | files: Map.put(new_fs.files, path, {content, 0o644})}
    {new_fs, {:ok, byte_size(content)}}
  end

  defp dispatch(fs, :mkdir, [path | _], kwargs) do
    path = extract_path(path)
    parents = kwargs["parents"] == true
    exist_ok = kwargs["exist_ok"] == true

    cond do
      MapSet.member?(fs.dirs, path) ->
        if exist_ok, do: {:ok, nil}, else: {:error, :o_s_error, "[Errno 17] File exists: '#{path}'"}

      parents ->
        {%{fs | dirs: MapSet.put(ensure_parent_dirs(fs, path <> "/x").dirs, path)}, {:ok, nil}}

      true ->
        parent = Path.dirname(path)

        if MapSet.member?(fs.dirs, parent) do
          {%{fs | dirs: MapSet.put(fs.dirs, path)}, {:ok, nil}}
        else
          {:error, :file_not_found_error,
           "[Errno 2] No such file or directory: '#{path}'"}
        end
    end
  end

  defp dispatch(fs, :unlink, [path | _], _kwargs) do
    path = extract_path(path)

    if Map.has_key?(fs.files, path) do
      {%{fs | files: Map.delete(fs.files, path)}, {:ok, nil}}
    else
      file_not_found(path)
    end
  end

  defp dispatch(fs, :rmdir, [path | _], _kwargs) do
    path = extract_path(path)

    if not MapSet.member?(fs.dirs, path) do
      file_not_found(path)
    else
      children =
        Enum.any?(fs.files, fn {p, _} -> String.starts_with?(p, path <> "/") end) or
          Enum.any?(fs.dirs, fn d -> d != path and String.starts_with?(d, path <> "/") end)

      if children do
        {:error, :o_s_error, "[Errno 39] Directory not empty: '#{path}'"}
      else
        {%{fs | dirs: MapSet.delete(fs.dirs, path)}, {:ok, nil}}
      end
    end
  end

  defp dispatch(fs, :iterdir, [path | _], _kwargs) do
    path = extract_path(path)

    if not MapSet.member?(fs.dirs, path) do
      file_not_found(path)
    else
      prefix = if String.ends_with?(path, "/"), do: path, else: path <> "/"

      file_entries =
        fs.files
        |> Map.keys()
        |> Enum.filter(fn p ->
          String.starts_with?(p, prefix) and
            not String.contains?(String.trim_leading(p, prefix), "/")
        end)

      dir_entries =
        fs.dirs
        |> Enum.filter(fn d ->
          d != path and String.starts_with?(d, prefix) and
            not String.contains?(String.trim_leading(d, prefix), "/")
        end)

      entries = Enum.map(file_entries ++ dir_entries, fn e -> {:path, e} end)
      {:ok, entries}
    end
  end

  defp dispatch(fs, :stat, [path | _], _kwargs) do
    path = extract_path(path)

    cond do
      Map.has_key?(fs.files, path) ->
        {content, mode} = Map.fetch!(fs.files, path)
        stat_mode = if mode < 0o1000, do: Bitwise.bor(mode, 0o100_000), else: mode
        {:ok, stat_result(stat_mode, byte_size(content), fs.mtime)}

      MapSet.member?(fs.dirs, path) ->
        dir_mode = Bitwise.bor(0o755, 0o040_000)
        {:ok, stat_result(dir_mode, 4096, fs.mtime)}

      true ->
        file_not_found(path)
    end
  end

  defp dispatch(fs, :rename, [path, target | _], _kwargs) do
    path = extract_path(path)
    target = extract_path(target)

    cond do
      Map.has_key?(fs.files, path) ->
        {content, mode} = Map.fetch!(fs.files, path)
        new_fs = ensure_parent_dirs(fs, target)

        new_fs = %{
          new_fs
          | files: new_fs.files |> Map.delete(path) |> Map.put(target, {content, mode})
        }

        {new_fs, {:ok, {:path, target}}}

      MapSet.member?(fs.dirs, path) ->
        new_fs = %{
          fs
          | dirs: fs.dirs |> MapSet.delete(path) |> MapSet.put(target)
        }

        {new_fs, {:ok, {:path, target}}}

      true ->
        file_not_found(path)
    end
  end

  defp dispatch(_fs, :resolve, [path | _], _kwargs) do
    {:ok, extract_path(path)}
  end

  defp dispatch(_fs, :absolute, [path | _], _kwargs) do
    {:ok, extract_path(path)}
  end

  defp dispatch(_fs, function, _args, _kwargs) do
    {:error, :not_implemented_error, "OS operation '#{function}' is not supported by PseudoFS"}
  end

  # ── Helpers ─────────────────────────────────────────────────────────────

  defp extract_path({:path, p}), do: p
  defp extract_path(p) when is_binary(p), do: p

  defp file_not_found(path) do
    {:error, :file_not_found_error, "[Errno 2] No such file or directory: '#{path}'"}
  end

  defp ensure_parent_dirs(%__MODULE__{} = fs, path) do
    parts = path |> Path.dirname() |> Path.split()

    {fs, _} =
      Enum.reduce(parts, {fs, ""}, fn part, {fs, acc} ->
        dir = if acc == "", do: part, else: acc <> "/" <> part
        dir = String.replace(dir, "//", "/")
        {%{fs | dirs: MapSet.put(fs.dirs, dir)}, dir}
      end)

    fs
  end

  defp stat_result(mode, size, mtime) do
    {:named_tuple, "StatResult",
     [
       {"st_mode", mode},
       {"st_ino", 0},
       {"st_dev", 0},
       {"st_nlink", if(Bitwise.band(mode, 0o040_000) != 0, do: 2, else: 1)},
       {"st_uid", 0},
       {"st_gid", 0},
       {"st_size", size},
       {"st_atime", mtime},
       {"st_mtime", mtime},
       {"st_ctime", mtime}
     ]}
  end
end
