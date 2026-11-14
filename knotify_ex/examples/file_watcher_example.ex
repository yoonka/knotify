defmodule FileWatcherExample do
  @moduledoc """
  Example implementation of a GenServer that uses KnotifyEx to watch files
  and perform actions based on file system events.

  This example watches a directory for changes and:
  - Logs all file events
  - Tracks file statistics (creates, modifies, deletes)
  - Maintains a list of recently changed files
  - Demonstrates event filtering

  ## Usage

      # Start the example watcher
      {:ok, pid} = FileWatcherExample.start_link(watch_dir: "/path/to/watch")

      # Get current statistics
      FileWatcherExample.get_stats(pid)

      # Get recently changed files
      FileWatcherExample.get_recent_files(pid)

      # Stop the watcher
      FileWatcherExample.stop(pid)

  ## Example with filtering

      # Only watch for create and modify events
      {:ok, pid} = FileWatcherExample.start_link(
        watch_dir: "/path/to/watch",
        events: [:create, :modify]
      )
  """

  use GenServer
  require Logger

  defstruct [
    :watcher_pid,
    :watch_dir,
    stats: %{creates: 0, creates_folder: 0, modifies: 0, removes: 0, removes_folder: 0, renames: 0},
    recent_files: [],
    max_recent: 10
  ]

  ## Client API

  @doc """
  Starts the file watcher example.

  ## Options

    * `:watch_dir` - Directory to watch (required)
    * `:events` - List of events to filter (optional, defaults to all)
    * `:name` - Optional name to register the process
    * `:max_recent` - Maximum number of recent files to track (default: 10)
    * `:recursive` - Watch subdirectories recursively (default: true)
  """
  def start_link(opts) do
    {name, opts} = Keyword.pop(opts, :name)

    if name do
      GenServer.start_link(__MODULE__, opts, name: name)
    else
      GenServer.start_link(__MODULE__, opts)
    end
  end

  @doc """
  Gets the current file event statistics.

  Returns a map with counts for each event type:
  `%{creates: n, creates_folder: n, modifies: n, removes: n, removes_folder: n, renames: n}`
  """
  def get_stats(server) do
    GenServer.call(server, :get_stats)
  end

  @doc """
  Gets the list of recently changed files.

  Returns a list of `{path, events, timestamp}` tuples.
  """
  def get_recent_files(server) do
    GenServer.call(server, :get_recent_files)
  end

  @doc """
  Resets the statistics counters.
  """
  def reset_stats(server) do
    GenServer.call(server, :reset_stats)
  end

  @doc """
  Stops the file watcher.
  """
  def stop(server) do
    GenServer.stop(server)
  end

  ## GenServer Callbacks

  @impl true
  def init(opts) do
    watch_dir = Keyword.fetch!(opts, :watch_dir)
    events = Keyword.get(opts, :events)
    max_recent = Keyword.get(opts, :max_recent, 10)
    recursive = Keyword.get(opts, :recursive, true)

    # Validate the watch directory exists
    unless File.dir?(watch_dir) do
      {:stop, {:invalid_directory, watch_dir}}
    else
      # Start the KnotifyEx watcher
      knotify_opts = [
        dirs: [watch_dir],
        recursive: recursive,
        debounce: 100
      ]

      case KnotifyEx.start_link(knotify_opts) do
        {:ok, watcher_pid} ->
          # Subscribe to events
          subscribe_opts = if events, do: [events: events], else: []
          KnotifyEx.subscribe(watcher_pid, subscribe_opts)

          Logger.info("Started file watcher for directory: #{watch_dir}")

          state = %__MODULE__{
            watcher_pid: watcher_pid,
            watch_dir: watch_dir,
            max_recent: max_recent
          }

          {:ok, state}

        {:error, reason} ->
          {:stop, reason}
      end
    end
  end

  @impl true
  def handle_call(:get_stats, _from, state) do
    {:reply, state.stats, state}
  end

  @impl true
  def handle_call(:get_recent_files, _from, state) do
    {:reply, Enum.reverse(state.recent_files), state}
  end

  @impl true
  def handle_call(:reset_stats, _from, state) do
    new_stats = %{creates: 0, creates_folder: 0, modifies: 0, removes: 0, removes_folder: 0, renames: 0}
    {:reply, :ok, %{state | stats: new_stats, recent_files: []}}
  end

  @impl true
  def handle_info({:file_event, watcher_pid, {path, events}}, %{watcher_pid: watcher_pid} = state) do
    Logger.info("File event: #{path} - #{inspect(events)}")

    # Update statistics
    new_stats = update_stats(state.stats, events)

    # Add to recent files (with timestamp)
    recent_entry = {path, events, DateTime.utc_now()}
    new_recent = add_recent_file(state.recent_files, recent_entry, state.max_recent)

    # Optionally process the file based on event type
    process_file_event(path, events)

    {:noreply, %{state | stats: new_stats, recent_files: new_recent}}
  end

  @impl true
  def handle_info({:file_event, watcher_pid, :stop}, %{watcher_pid: watcher_pid} = state) do
    Logger.warning("File watcher stopped unexpectedly")
    {:stop, :watcher_stopped, state}
  end

  @impl true
  def handle_info(msg, state) do
    Logger.debug("Unexpected message: #{inspect(msg)}")
    {:noreply, state}
  end

  @impl true
  def terminate(reason, state) do
    Logger.info("Stopping file watcher, reason: #{inspect(reason)}")

    if state.watcher_pid && Process.alive?(state.watcher_pid) do
      KnotifyEx.stop(state.watcher_pid)
    end

    :ok
  end

  ## Private Functions

  defp update_stats(stats, events) do
    Enum.reduce(events, stats, fn event, acc ->
      case event do
        :create -> Map.update!(acc, :creates, &(&1 + 1))
        :create_folder -> Map.update!(acc, :creates_folder, &(&1 + 1))
        :modify -> Map.update!(acc, :modifies, &(&1 + 1))
        :remove -> Map.update!(acc, :removes, &(&1 + 1))
        :remove_folder -> Map.update!(acc, :removes_folder, &(&1 + 1))
        :rename -> Map.update!(acc, :renames, &(&1 + 1))
      end
    end)
  end

  defp add_recent_file(recent_files, new_entry, max_recent) do
    # Add to front and trim to max_recent size
    [new_entry | recent_files]
    |> Enum.take(max_recent)
  end

  defp process_file_event(path, events) do
    # Example processing logic - customize based on your needs
    cond do
      :create in events ->
        handle_file_created(path)

      :create_folder in events ->
        handle_folder_created(path)

      :modify in events ->
        handle_file_modified(path)

      :remove in events ->
        handle_file_removed(path)

      :remove_folder in events ->
        handle_folder_removed(path)

      :rename in events ->
        handle_file_renamed(path)

      true ->
        :ok
    end
  end

  defp handle_file_created(path) do
    # Example: Process new files
    Logger.debug("New file created: #{path}")

    # You could:
    # - Validate file format
    # - Parse and process the file
    # - Move it to another location
    # - Send a notification
  end

  defp handle_file_modified(path) do
    # Example: Handle file modifications
    Logger.debug("File modified: #{path}")

    # You could:
    # - Reload configuration if it's a config file
    # - Reprocess the file
    # - Update cache
  end

  defp handle_file_removed(path) do
    # Example: Handle file deletions
    Logger.debug("File removed: #{path}")

    # You could:
    # - Clean up related resources
    # - Update indexes
    # - Archive or backup
  end

  defp handle_file_renamed(path) do
    # Example: Handle file renames
    Logger.debug("File renamed: #{path}")

    # You could:
    # - Update references
    # - Reindex
    # - Update database records
  end

  defp handle_folder_created(path) do
    # Example: Handle folder creation
    Logger.debug("New folder created: #{path}")

    # You could:
    # - Set up watchers for the new folder
    # - Initialize folder-specific resources
    # - Create metadata or index entries
  end

  defp handle_folder_removed(path) do
    # Example: Handle folder deletion
    Logger.debug("Folder removed: #{path}")

    # You could:
    # - Clean up folder-specific resources
    # - Remove all file references in the folder
    # - Archive or log the deletion
  end
end
