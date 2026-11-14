defmodule KnotifyEx do
  @moduledoc """
  A fast, reliable file system watcher for Elixir.

  KnotifyEx provides real-time notifications when files and directories change on your file system.
  It's built on top of the `knotify` binary, which wraps Rust's excellent `notify` crate, giving you
  the performance of Rust with the convenience of Elixir.

  ## Features

  - **Cross-platform**: Works on Linux (inotify), macOS (FSEvents), and other Unix systems (kqueue)
  - **Event filtering**: Subscribe only to the events you care about
  - **Debouncing**: Built-in debouncing to prevent event flooding
  - **Multiple watchers**: Run multiple independent watchers in the same application
  - **Supervision-friendly**: Designed to work seamlessly with OTP supervision trees

  ## Getting Started

  The basic workflow is simple:

  1. Start a watcher with `start_link/1`
  2. Subscribe to events with `subscribe/2`
  3. Handle file event messages in your process

  ### Quick Example

      # Start a watcher
      {:ok, watcher} = KnotifyEx.start_link(dirs: ["./lib"])

      # Subscribe to all events
      KnotifyEx.subscribe(watcher)

      # Receive messages like:
      # {:file_event, watcher_pid, {"/path/to/file.ex", [:modify]}}

  ### Event Types

  KnotifyEx recognizes six types of file system events:

  - `:create` - A new file was created
  - `:create_folder` - A new directory was created
  - `:modify` - A file's contents changed
  - `:remove` - A file was deleted
  - `:remove_folder` - A directory was deleted
  - `:rename` - A file or directory was renamed

  ### Filtering Events

  You can subscribe to specific event types to reduce noise:

      # Only notify about file creation and modification
      KnotifyEx.subscribe(watcher, events: [:create, :modify])

      # Only track directory changes
      KnotifyEx.subscribe(watcher, events: [:create_folder, :remove_folder])

  ## Working with GenServers

  KnotifyEx works great with GenServers. Here's a complete example:

      defmodule MyApp.FileWatcher do
        use GenServer
        require Logger

        def start_link(opts) do
          GenServer.start_link(__MODULE__, opts, name: __MODULE__)
        end

        def init(opts) do
          dirs = Keyword.get(opts, :dirs, ["./lib"])

          # Start the watcher
          {:ok, watcher} = KnotifyEx.start_link(
            dirs: dirs,
            recursive: true,
            debounce: 100
          )

          # Subscribe to events
          KnotifyEx.subscribe(watcher, events: [:create, :modify])

          {:ok, %{watcher: watcher}}
        end

        def handle_info({:file_event, _watcher, {path, events}}, state) do
          Logger.info("File changed: \#{path}")
          Logger.debug("Events: \#{inspect(events)}")
          {:noreply, state}
        end

        def handle_info({:file_event, _watcher, :stop}, state) do
          Logger.warning("Watcher stopped")
          {:noreply, state}
        end
      end

  ## Message Format

  Subscribed processes receive messages in this format:

      # When a file event occurs
      {:file_event, watcher_pid, {file_path, events}}

      # When the watcher stops
      {:file_event, watcher_pid, :stop}

  The `events` field is always a list, even if only one event type occurred.

  ## Configuration Options

  When starting a watcher, you can configure:

  - `:dirs` (required) - List of directories to watch
  - `:name` (optional) - Atom to register the watcher process
  - `:recursive` (default: `true`) - Watch subdirectories recursively
  - `:backend` (default: `:auto`) - File watching backend (`:auto`, `:inotify`, `:kqueue`, `:fsevent`)
  - `:debounce` (default: `100`) - Debounce delay in milliseconds

  See `start_link/1` for detailed examples.

  ## Multiple Watchers

  You can run multiple independent watchers in the same application:

      # Watch source code
      {:ok, code_watcher} = KnotifyEx.start_link(
        dirs: ["./lib"],
        name: :code_watcher
      )

      # Watch configuration files
      {:ok, config_watcher} = KnotifyEx.start_link(
        dirs: ["./config"],
        name: :config_watcher
      )

      # Subscribe to different events from each
      KnotifyEx.subscribe(:code_watcher, events: [:create, :modify])
      KnotifyEx.subscribe(:config_watcher, events: [:modify])

  ## Prerequisites

  KnotifyEx requires the `knotify` binary to be installed and available in your system PATH.


  ## Performance Tips

  - Use event filtering to only receive events you need
  - Adjust debounce timing based on your use case (higher for rapidly changing files)
  - Watch specific directories rather than entire file systems
  - Be mindful of recursive watching on large directory trees
  """

  @type event :: :create | :create_folder | :modify | :remove | :remove_folder | :rename
  @type file_path :: String.t()
  @type watcher :: pid() | atom()

  @doc """
  Starts a file system watcher process.

  This function starts a new watcher that monitors the specified directories for file system changes.
  The watcher runs as a GenServer and can be supervised like any other OTP process.

  ## Options

    * `:dirs` (required) - A list of directory paths to watch. Each directory must exist or the watcher
      will fail to start. Paths can be relative or absolute.

    * `:name` (optional) - An atom to register the watcher process with. If provided, you can reference
      the watcher by name instead of keeping track of its PID. This is useful when integrating with
      supervision trees.

    * `:recursive` (optional, default: `true`) - When `true`, the watcher will monitor all subdirectories
      recursively. When `false`, only the top-level directory is watched.

    * `:backend` (optional, default: `:auto`) - The file watching backend to use. Options are:
      - `:auto` - Automatically choose the best backend for your operating system (recommended)
      - `:inotify` - Linux inotify (Linux only)
      - `:kqueue` - BSD kqueue (macOS, BSD, etc.)
      - `:fsevent` - macOS FSEvents (macOS only)

    * `:debounce` (optional, default: `100`) - Debounce delay in milliseconds. File system events that
      occur within this time window will be coalesced to prevent your application from being overwhelmed
      by rapid-fire events. A value of 100-500ms is typical.

  ## Return Value

  Returns `{:ok, pid}` on success, where `pid` is the process identifier of the watcher.

  Returns `{:error, reason}` if the watcher fails to start. Common reasons include:
    - Invalid directories (directory doesn't exist)
    - Missing knotify binary
    - Permission issues

  ## Examples

      # Watch a single directory
      {:ok, watcher} = KnotifyEx.start_link(dirs: ["./lib"])

      # Watch multiple directories
      {:ok, watcher} = KnotifyEx.start_link(dirs: ["./lib", "./config", "./priv"])

      # Register with a name for easy reference
      {:ok, watcher} = KnotifyEx.start_link(
        dirs: ["./lib"],
        name: :code_watcher
      )

      # Only watch the top-level directory (non-recursive)
      {:ok, watcher} = KnotifyEx.start_link(
        dirs: ["/var/log"],
        recursive: false
      )

      # Use a specific backend and custom debounce
      {:ok, watcher} = KnotifyEx.start_link(
        dirs: ["./lib"],
        backend: :inotify,
        debounce: 500
      )

      # Complete configuration example
      {:ok, watcher} = KnotifyEx.start_link(
        dirs: ["./lib", "./test"],
        name: :my_watcher,
        recursive: true,
        backend: :auto,
        debounce: 250
      )

  ## Supervision

  KnotifyEx watchers work great in supervision trees:

      children = [
        {KnotifyEx, dirs: ["./lib"], name: :code_watcher}
      ]

      Supervisor.start_link(children, strategy: :one_for_one)

  """
  @spec start_link(keyword()) :: GenServer.on_start()
  def start_link(opts) do
    {name, opts} = Keyword.pop(opts, :name)

    if name do
      KnotifyEx.Worker.start_link(opts, name: name)
    else
      KnotifyEx.Worker.start_link(opts)
    end
  end

  @doc """
  Acknowledge an ETF hint batch after adding its paths to bounded intake.

  Start with `protocol: :etf, executable: absolute_path`. A single collector
  subscribes and receives `{:knotify, watcher, message}` with binary-keyed
  protocol-v1 maps. Scoped watch errors use `rescan` with an optional `paths`
  list; rescans without it cover the root. `paths` and `rescan` messages carry `batch_id`; acknowledge
  both. Paths are exact root-relative bytes (Unix) or WTF-8 (Windows).
  Readiness is replayed to late subscribers. Legacy JSON messages are unchanged.
  """
  @spec ack(GenServer.server(), non_neg_integer()) :: :ok | {:error, term()}
  def ack(watcher, batch_id), do: GenServer.call(watcher, {:ack, batch_id})

  @doc """
  Subscribes the calling process to receive file system events from a watcher.

  Once subscribed, your process will receive messages whenever files or directories change
  in the watched directories. The watcher will automatically monitor your process and clean
  up the subscription if your process terminates.

  ## Options

    * `:events` (optional) - A list of specific event types you want to receive. If not provided,
      you'll receive all event types. Available events are:
      - `:create` - File created
      - `:create_folder` - Directory created
      - `:modify` - File modified
      - `:remove` - File deleted
      - `:remove_folder` - Directory deleted
      - `:rename` - File or directory renamed

  ## Message Format

  After subscribing, you'll receive messages in this format:

      {:file_event, watcher_pid, {path, events}}

  Where:
    - `watcher_pid` is the PID of the watcher that sent the event
    - `path` is the full path to the file or directory that changed
    - `events` is a list of event atoms (e.g., `[:modify]`)

  When the watcher stops, you'll receive:

      {:file_event, watcher_pid, :stop}

  ## Examples

      # Subscribe to all events
      KnotifyEx.subscribe(watcher)

      # Subscribe to file creation and modification only
      KnotifyEx.subscribe(watcher, events: [:create, :modify])

      # Subscribe to directory events only
      KnotifyEx.subscribe(watcher, events: [:create_folder, :remove_folder])

      # Subscribe using a named watcher
      {:ok, _} = KnotifyEx.start_link(dirs: ["./lib"], name: :my_watcher)
      KnotifyEx.subscribe(:my_watcher, events: [:modify])

  ## Handling Events

  In a GenServer:

      def handle_info({:file_event, _watcher, {path, events}}, state) do
        IO.puts("File changed: \#{path}")
        IO.inspect(events, label: "Events")
        {:noreply, state}
      end

      def handle_info({:file_event, _watcher, :stop}, state) do
        # Watcher stopped - maybe restart it?
        {:noreply, state}
      end

  In a regular process:

      KnotifyEx.subscribe(watcher)

      receive do
        {:file_event, _watcher, {path, events}} ->
          IO.puts("Got event: \#{inspect(events)} for \#{path}")
      end

  ## Multiple Subscribers

  Multiple processes can subscribe to the same watcher, each with their own event filters:

      # Process A only cares about modifications
      KnotifyEx.subscribe(watcher, events: [:modify])

      # Process B wants all events
      spawn(fn ->
        KnotifyEx.subscribe(watcher)
        # ... handle events
      end)

  """
  @spec subscribe(watcher(), keyword()) :: :ok
  def subscribe(watcher, opts \\ []) do
    # Validate in the caller so invalid options do not crash a linked watcher.
    KnotifyEx.Worker.validate_subscription!(opts)
    GenServer.call(watcher, {:subscribe, self(), opts})
  end

  @doc """
  Unsubscribes the calling process from file system events.

  After unsubscribing, your process will no longer receive file event messages from this watcher.

  Note: You don't need to manually unsubscribe when your process terminates - the watcher
  automatically monitors all subscribers and cleans up subscriptions when a process dies.

  ## Examples

      # Unsubscribe from a watcher
      KnotifyEx.unsubscribe(watcher)

      # Unsubscribe from a named watcher
      KnotifyEx.unsubscribe(:my_watcher)

  ## Use Cases

  Unsubscribing is useful when:
    - You want to temporarily stop receiving events without terminating your process
    - You're implementing dynamic event filtering and need to change subscriptions
    - You're cleaning up before doing intensive work that shouldn't be interrupted

  """
  @spec unsubscribe(watcher()) :: :ok
  def unsubscribe(watcher) do
    GenServer.call(watcher, {:unsubscribe, self()})
  end

  @doc """
  Returns the list of directories currently being watched.

  This is useful for debugging or displaying the current watch configuration.

  ## Examples

      {:ok, watcher} = KnotifyEx.start_link(dirs: ["./lib", "./config"])
      KnotifyEx.known_dirs(watcher)
      #=> ["./lib", "./config"]

      # With a named watcher
      {:ok, _} = KnotifyEx.start_link(dirs: ["/tmp", "/var/log"], name: :log_watcher)
      KnotifyEx.known_dirs(:log_watcher)
      #=> ["/tmp", "/var/log"]

  """
  @spec known_dirs(watcher()) :: [file_path()]
  def known_dirs(watcher) do
    GenServer.call(watcher, :known_dirs)
  end

  @doc """
  Stops the file system watcher gracefully.

  When a watcher is stopped:
    1. It closes the connection to the knotify binary
    2. All subscribers receive a `{:file_event, watcher_pid, :stop}` message
    3. The watcher process terminates

  If the watcher is part of a supervision tree, you probably don't need to call this directly -
  just let the supervisor handle the lifecycle.

  ## Examples

      # Stop a watcher
      {:ok, watcher} = KnotifyEx.start_link(dirs: ["./lib"])
      KnotifyEx.stop(watcher)

      # Stop a named watcher
      {:ok, _} = KnotifyEx.start_link(dirs: ["./lib"], name: :my_watcher)
      KnotifyEx.stop(:my_watcher)

  """
  @spec stop(watcher()) :: :ok
  def stop(watcher) do
    GenServer.stop(watcher)
  end
end
