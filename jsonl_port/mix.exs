defmodule JsonlPort.MixProject do
  use Mix.Project

  @version "1.1.0"
  @source_url "https://github.com/yoonka/beamports"

  def project do
    [
      app: :jsonl_port,
      version: @version,
      elixir: "~> 1.16",
      start_permanent: Mix.env() == :prod,
      deps: deps(),
      description: description(),
      package: package(),
      docs: docs(),
      name: "JsonlPort",
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
      {:jason, "~> 1.4"},
      {:ex_doc, "~> 0.31", only: :dev, runtime: false}
    ]
  end

  defp description do
    """
    Runs a program that speaks the beamports JSON-lines request protocol
    (duckport, oxiport, tantiport) as an Erlang port, one request at a time,
    with correlated replies, timeouts and size limits.
    """
  end

  defp package do
    [
      name: "jsonl_port",
      files: ~w(lib .formatter.exs mix.exs README.md LICENSE),
      licenses: ["BSD-3-Clause"],
      links: %{
        "GitHub" => @source_url
      },
      maintainers: ["Ahmed Lukman"]
    ]
  end

  defp docs do
    [
      main: "readme",
      extras: [
        "README.md": [title: "Overview"]
      ],
      source_url: @source_url,
      source_url_pattern: "#{@source_url}/blob/v#{@version}/jsonl_port/%{path}#L%{line}"
    ]
  end
end
