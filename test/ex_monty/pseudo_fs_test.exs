defmodule ExMonty.PseudoFSTest do
  use ExUnit.Case

  alias ExMonty.PseudoFS

  describe "basic file operations" do
    test "read_text from virtual file" do
      fs =
        PseudoFS.new()
        |> PseudoFS.put_file("/data/hello.txt", "Hello, World!")

      {:ok, result, _output} =
        ExMonty.Sandbox.run(
          "from pathlib import Path\nPath('/data/hello.txt').read_text()",
          os: fs
        )

      assert result == "Hello, World!"
    end

    test "file exists check" do
      fs =
        PseudoFS.new()
        |> PseudoFS.put_file("/data/hello.txt", "content")

      {:ok, result, _} =
        ExMonty.Sandbox.run(
          "from pathlib import Path\nPath('/data/hello.txt').exists()",
          os: fs
        )

      assert result == true
    end

    test "file does not exist" do
      fs = PseudoFS.new()

      {:ok, result, _} =
        ExMonty.Sandbox.run(
          "from pathlib import Path\nPath('/missing.txt').exists()",
          os: fs
        )

      assert result == false
    end

    test "is_file check" do
      fs =
        PseudoFS.new()
        |> PseudoFS.put_file("/data/file.txt", "content")
        |> PseudoFS.mkdir("/data/dir")

      code = """
      from pathlib import Path
      [Path('/data/file.txt').is_file(), Path('/data/dir').is_file()]
      """

      {:ok, result, _} = ExMonty.Sandbox.run(code, os: fs)
      assert result == [true, false]
    end

    test "is_dir check" do
      fs =
        PseudoFS.new()
        |> PseudoFS.put_file("/data/file.txt", "content")
        |> PseudoFS.mkdir("/data/dir")

      code = """
      from pathlib import Path
      [Path('/data/file.txt').is_dir(), Path('/data/dir').is_dir()]
      """

      {:ok, result, _} = ExMonty.Sandbox.run(code, os: fs)
      assert result == [false, true]
    end

    test "read nonexistent file raises FileNotFoundError" do
      fs = PseudoFS.new()

      code = """
      from pathlib import Path
      try:
          result = Path('/missing.txt').read_text()
      except FileNotFoundError as e:
          result = str(e)
      result
      """

      {:ok, result, _} = ExMonty.Sandbox.run(code, os: fs)
      assert result =~ "No such file or directory"
    end
  end

  describe "write operations" do
    test "write_text creates file" do
      fs = PseudoFS.new() |> PseudoFS.mkdir("/data")

      code = """
      from pathlib import Path
      Path('/data/output.txt').write_text('hello')
      Path('/data/output.txt').read_text()
      """

      {:ok, result, _} = ExMonty.Sandbox.run(code, os: fs)
      assert result == "hello"
    end

    test "write then read back" do
      fs = PseudoFS.new() |> PseudoFS.mkdir("/tmp")

      code = """
      from pathlib import Path
      p = Path('/tmp/test.txt')
      p.write_text('written content')
      p.read_text()
      """

      {:ok, result, _} = ExMonty.Sandbox.run(code, os: fs)
      assert result == "written content"
    end
  end

  describe "directory operations" do
    test "iterdir lists directory contents" do
      fs =
        PseudoFS.new()
        |> PseudoFS.put_file("/data/a.txt", "a")
        |> PseudoFS.put_file("/data/b.txt", "b")

      code = """
      from pathlib import Path
      [str(p) for p in Path('/data').iterdir()]
      """

      {:ok, result, _} = ExMonty.Sandbox.run(code, os: fs)
      assert Enum.sort(result) == ["/data/a.txt", "/data/b.txt"]
    end

    test "mkdir creates directory" do
      fs = PseudoFS.new() |> PseudoFS.mkdir("/parent")

      code = """
      from pathlib import Path
      Path('/parent/child').mkdir()
      Path('/parent/child').is_dir()
      """

      {:ok, result, _} = ExMonty.Sandbox.run(code, os: fs)
      assert result == true
    end
  end

  describe "environment variables" do
    test "os.getenv reads virtual env" do
      fs =
        PseudoFS.new()
        |> PseudoFS.put_env("API_KEY", "secret123")

      code = """
      import os
      os.getenv('API_KEY')
      """

      {:ok, result, _} = ExMonty.Sandbox.run(code, os: fs)
      assert result == "secret123"
    end

    test "os.getenv returns default for missing key" do
      fs = PseudoFS.new()

      code = """
      import os
      os.getenv('MISSING', 'default_value')
      """

      {:ok, result, _} = ExMonty.Sandbox.run(code, os: fs)
      assert result == "default_value"
    end

    test "os.environ returns full env dict" do
      fs =
        PseudoFS.new()
        |> PseudoFS.put_env("KEY1", "val1")
        |> PseudoFS.put_env("KEY2", "val2")

      code = """
      import os
      dict(os.environ)
      """

      {:ok, result, _} = ExMonty.Sandbox.run(code, os: fs)
      assert result["KEY1"] == "val1"
      assert result["KEY2"] == "val2"
    end
  end

  describe "stat operations" do
    test "stat returns file info" do
      fs =
        PseudoFS.new()
        |> PseudoFS.put_file("/data/file.txt", "hello")

      code = """
      from pathlib import Path
      s = Path('/data/file.txt').stat()
      s.st_size
      """

      {:ok, result, _} = ExMonty.Sandbox.run(code, os: fs)
      assert result == 5
    end
  end

  describe "PseudoFS builder" do
    test "new creates empty fs" do
      fs = PseudoFS.new()
      assert fs.files == %{}
      assert fs.env == %{}
    end

    test "put_file adds files" do
      fs =
        PseudoFS.new()
        |> PseudoFS.put_file("/a.txt", "content a")
        |> PseudoFS.put_file("/b.txt", "content b")

      assert map_size(fs.files) == 2
    end

    test "put_file auto-creates parent dirs" do
      fs = PseudoFS.new() |> PseudoFS.put_file("/a/b/c/file.txt", "deep")
      assert MapSet.member?(fs.dirs, "/a")
      assert MapSet.member?(fs.dirs, "/a/b")
      assert MapSet.member?(fs.dirs, "/a/b/c")
    end

    test "put_envs sets multiple env vars" do
      fs = PseudoFS.new() |> PseudoFS.put_envs(%{"A" => "1", "B" => "2"})
      assert fs.env["A"] == "1"
      assert fs.env["B"] == "2"
    end
  end
end
