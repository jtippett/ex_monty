defmodule ExMonty.MixProject do
  use Mix.Project

  @version "0.5.2"
  @source_url "https://github.com/jtippett/ex_monty"

  def project do
    [
      app: :ex_monty,
      version: @version,
      elixir: "~> 1.15",
      start_permanent: Mix.env() == :prod,
      deps: deps(),
      package: package(),
      docs: docs(),
      name: "ExMonty",
      description:
        "Elixir NIF wrapper for Monty, a minimal secure Python interpreter written in Rust",
      source_url: @source_url
    ]
  end

  def application do
    [
      extra_applications: [:logger]
    ]
  end

  defp deps do
    [
      {:rustler, "~> 0.37", optional: true},
      {:rustler_precompiled, "~> 0.8"},
      {:ex_doc, "~> 0.34", only: :dev, runtime: false}
    ]
  end

  defp package do
    [
      licenses: ["MIT"],
      links: %{"GitHub" => @source_url},
      files:
        ~w(lib native/ex_monty/Cargo.toml native/ex_monty/Cargo.lock native/ex_monty/src checksum-Elixir.ExMonty.Native.exs .formatter.exs mix.exs README.md CHANGELOG.md UPDATE_PROCEDURE.md LICENSE)
    ]
  end

  defp docs do
    [
      main: "readme",
      extras: ["README.md", "CHANGELOG.md", "UPDATE_PROCEDURE.md"],
      source_url: @source_url,
      source_ref: "v#{@version}"
    ]
  end
end
