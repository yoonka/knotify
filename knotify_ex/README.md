# KnotifyEx

## Lossless hint mode (1.1)

Use the matching knotify 1.1 executable for an acknowledged, bounded hint stream:

```elixir
{:ok, watcher} = KnotifyEx.start_link(
  dirs: [root], executable: absolute_knotify_path, protocol: :etf, recursive: true
)
:ok = KnotifyEx.subscribe(watcher)

receive do
  {:knotify, ^watcher, %{"type" => "paths", "batch_id" => id, "paths" => paths}} ->
    # Add paths to bounded intake; inspect disk and catalog asynchronously.
    accept_paths(paths)
    :ok = KnotifyEx.ack(watcher, id)
  {:knotify, ^watcher, %{"type" => "rescan", "batch_id" => id}} ->
    schedule_root_rescan()
    :ok = KnotifyEx.ack(watcher, id)
end
```

Only one root and one collector are supported in hint mode; event-type filters
are not supported. Paths are exact root-relative byte binaries, using WTF-8 on
Windows. Preserve them as opaque binaries. Readiness (`type: "ready"`) is cached
and replayed to late subscribers. Registration/protocol errors are structured
`type: "error"` messages. At most four path/rescan batches may be outstanding;
acknowledge only after accepting work into bounded intake. Overflow requests a
root rescan. Restarting a watcher also requires reconciliation.

The default remains legacy JSON and `{:file_event, watcher, {path, events}}`.
`:executable` also works with the legacy protocol, without changing `PATH`.
Wrapped native versions supporting `KNOTIFY_EXIT_ON_EOF=1` exit when their port
closes; older binaries ignore this environment variable.

A fast, reliable file system watcher for Elixir powered by [knotify](https://github.com/yoonka/beamports/tree/master/knotify), which wraps Rust's excellent [notify](https://github.com/notify-rs/notify) crate.

KnotifyEx gives you real-time notifications when files and directories change. It's cross-platform, efficient, and works great in development and production environments.

## Why KnotifyEx?

- **Fast and Efficient**: Built on Rust's notify crate, one of the fastest file watching libraries available
- **Cross-Platform**: Native support for Linux (inotify), macOS (FSEvents), and other Unix systems (kqueue)
- **Simple API**: Subscribe to file events with just a few lines of code
- **Flexible Filtering**: Watch only the events you care about
- **Battle-Tested**: Leverages the mature Rust ecosystem for reliability

## Prerequisites

KnotifyEx requires the `knotify` binary to be installed on your system and available in your PATH.

### Installing knotify

Pre-built binaries are available on the [beamports releases](https://github.com/yoonka/beamports/releases) page. Download the appropriate binary for your platform, extract, and add it to your system PATH.

Verify installation:
```bash
which knotify
# Should output: /usr/local/bin/knotify (or similar)
```

## Installation

Add `knotify_ex` to your dependencies in `mix.exs`:

```elixir
def deps do
  [
    {:knotify_ex, "~> 1.1"}
  ]
end
```

Then run:
```bash
mix deps.get
```

## Quick Start

The simplest way to use KnotifyEx is to start a watcher and subscribe to events:

```elixir
# Start watching a directory
{:ok, watcher} = KnotifyEx.start_link(dirs: ["./lib"])

# Subscribe to all file events
KnotifyEx.subscribe(watcher)

# You'll receive messages like:
# {:file_event, watcher_pid, {"/path/to/file.ex", [:modify]}}
```

### Event Types

KnotifyEx recognizes these file system events:

- `:create` - A new file was created
- `:create_folder` - A new directory was created
- `:modify` - A file's contents changed
- `:remove` - A file was deleted
- `:remove_folder` - A directory was deleted
- `:rename` - A file or directory was renamed

### Filtering Events

Subscribe to only the events you care about:

```elixir
# Only notify me about new files and modifications
KnotifyEx.subscribe(watcher, events: [:create, :modify])

# Only track directory changes
KnotifyEx.subscribe(watcher, events: [:create_folder, :remove_folder])
```

## Real-World Example: Hot Reloading

Here's a complete example of a GenServer that watches for file changes and triggers recompilation:

```elixir
defmodule MyApp.DevReloader do
  use GenServer
  require Logger

  def start_link(opts) do
    GenServer.start_link(__MODULE__, opts, name: __MODULE__)
  end

  def init(opts) do
    dirs = Keyword.get(opts, :dirs, ["./lib", "./config"])

    # Start the file watcher
    {:ok, watcher} = KnotifyEx.start_link(
      dirs: dirs,
      recursive: true,
      debounce: 100
    )

    # Subscribe to file changes (only create and modify)
    KnotifyEx.subscribe(watcher, events: [:create, :modify])

    Logger.info("Watching #{length(dirs)} directories for changes...")
    {:ok, %{watcher: watcher, dirs: dirs}}
  end

  def handle_info({:file_event, _watcher, {path, events}}, state) do
    # Only reload for Elixir files
    if String.ends_with?(path, ".ex") or String.ends_with?(path, ".exs") do
      Logger.info("File changed: #{Path.relative_to_cwd(path)}")
      Logger.debug("Events: #{inspect(events)}")

      # Trigger your reload logic here
      IEx.Helpers.recompile()
    end

    {:noreply, state}
  end

  def handle_info({:file_event, _watcher, :stop}, state) do
    Logger.warning("File watcher stopped unexpectedly")
    {:noreply, state}
  end
end
```

Add it to your application supervision tree:

```elixir
# In your application.ex
def start(_type, _args) do
  children = [
    # Your other processes...
    {MyApp.DevReloader, dirs: ["./lib", "./config"]}
  ]

  opts = [strategy: :one_for_one, name: MyApp.Supervisor]
  Supervisor.start_link(children, opts)
end
```

## Configuration Options

When starting a watcher with `KnotifyEx.start_link/1`, you can pass these options:

### Required Options

- `:dirs` (list of strings) - One or more directories to watch. Each directory must exist.
  ```elixir
  dirs: ["./lib"]
  dirs: ["/var/log", "/etc/config"]
  ```

### Optional Options

- `:name` (atom) - Register the watcher with a name so you can reference it without keeping the PID.
  ```elixir
  {:ok, _pid} = KnotifyEx.start_link(dirs: ["./lib"], name: :my_watcher)
  KnotifyEx.subscribe(:my_watcher)
  ```

- `:recursive` (boolean, default: `true`) - Whether to watch subdirectories recursively.
  ```elixir
  # Only watch the top-level directory, not subdirectories
  recursive: false
  ```

- `:backend` (atom, default: `:auto`) - Which file watching backend to use. Options are:
  - `:auto` - Automatically choose the best backend for your OS (recommended)
  - `:inotify` - Linux inotify (Linux only)
  - `:kqueue` - BSD kqueue (macOS, BSD)
  - `:fsevent` - macOS FSEvents (macOS only)

  ```elixir
  # Force inotify on Linux
  backend: :inotify
  ```

- `:debounce` (integer, default: `100`) - Debounce delay in milliseconds. File system events that occur within this window will be coalesced to prevent flooding your application with duplicate events.
  ```elixir
  # Wait 500ms before sending events (useful for editors that save multiple times)
  debounce: 500
  ```

### Complete Example

```elixir
{:ok, watcher} = KnotifyEx.start_link(
  dirs: ["./lib", "./priv"],
  name: :my_app_watcher,
  recursive: true,
  backend: :auto,
  debounce: 250
)
```

## API Reference

### `start_link/1`

Starts a new file watcher process. Returns `{:ok, pid}` on success.

### `subscribe/2`

Subscribes the calling process to receive file events from the watcher.

Options:
- `:events` - List of event types to filter (optional). If omitted, all events are sent.

### `unsubscribe/1`

Unsubscribes the calling process from the watcher.

### `known_dirs/1`

Returns the list of directories currently being watched.

```elixir
KnotifyEx.known_dirs(watcher)
# => ["./lib", "./config"]
```

### `stop/1`

Stops the watcher process gracefully.

## Message Format

Subscribed processes receive messages in this format:

```elixir
# File event
{:file_event, watcher_pid, {path, events}}

# Watcher stopped
{:file_event, watcher_pid, :stop}
```

Example:
```elixir
{:file_event, #PID<0.123.0>, {"/Users/you/project/lib/my_module.ex", [:modify]}}
```

## Use Cases

KnotifyEx is perfect for:

- **Development tools** - Auto-reloading, hot code reloading
- **Build systems** - Triggering rebuilds when source files change
- **Testing** - Running tests automatically when files are saved
- **Log monitoring** - Watching log directories for new entries
- **Configuration reloading** - Reloading app config when files change
- **File processing pipelines** - Processing files as they arrive in a directory

## Performance Considerations

- **Debouncing**: Use a reasonable debounce value (100-500ms) to avoid overwhelming your system with events, especially when watching directories with frequent changes.
- **Event filtering**: Subscribe only to the events you need. This reduces message passing overhead.
- **Directory scope**: Watch specific directories rather than entire file systems. The more files you watch, the more system resources are used.
- **Recursive watching**: Be mindful when using `recursive: true` on large directory trees. Consider watching only the directories you need.

## Troubleshooting

### "Could not find knotify binary in system PATH"

Make sure `knotify` is installed and available in your PATH:
```bash
which knotify
# Should output the path to knotify
```

If not found, install it:
```bash
cargo install --git https://github.com/yoonka/beamports knotify
```

### Events not firing

1. Verify the directory exists and is readable
2. Check that you're subscribed to the watcher
3. Ensure the event type you're expecting is in your filter (if using event filtering)
4. Try increasing the debounce delay if events are happening too quickly

### Too many events

1. Increase the `:debounce` value to coalesce rapid events
2. Use event filtering to only receive relevant events
3. Consider watching a more specific directory

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

BSD 3-Clause License - see LICENSE file for details.

## Credits

KnotifyEx is powered by:
- [knotify](https://github.com/yoonka/beamports/tree/master/knotify) - CLI wrapper around Rust's notify
- [notify](https://github.com/notify-rs/notify) - Cross-platform file system notification library for Rust

