defmodule KnotifyEx.MixProject do
  use Mix.Project

  @version "1.1.0"
  @source_url "https://github.com/yoonka/beamports"

  def project do
    [
      app: :knotify_ex,
      version: @version,
      elixir: "~> 1.16",
      start_permanent: Mix.env() == :prod,
      deps: deps(),
      description: description(),
      package: package(),
      docs: docs(),
      name: "KnotifyEx",
      source_url: @source_url
    ]
  end

  # Run "mix help compile.app" to learn about applications.
  def application do
    [
      extra_applications: [:logger]
    ]
  end

  # Run "mix help deps" to learn about dependencies.
  defp deps do
    [
      {:jason, "~> 1.4"},
      {:ex_doc, "~> 0.31", only: :dev, runtime: false}
    ]
  end

  defp description do
    """
    A fast, reliable file system watcher for Elixir powered by Rust's notify crate.
    Cross-platform support for Linux (inotify), macOS (FSEvents), and other Unix systems (kqueue).
    """
  end

  defp package do
    [
      name: "knotify_ex",
      files: ~w(lib .formatter.exs mix.exs README.md LICENSE),
      licenses: ["BSD-3-Clause"],
      links: %{
        "GitHub" => @source_url
      },
      maintainers: ["Edwin Nguthiru"]
    ]
  end

  defp docs do
    [
      main: "readme",
      extras: [
        "README.md": [title: "Overview"]
      ],
      source_url: @source_url,
      source_url_pattern: "#{@source_url}/blob/v#{@version}/knotify_ex/%{path}#L%{line}",
      api_reference: false
    ]
  end
end
