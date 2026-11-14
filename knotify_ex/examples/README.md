# KnotifyEx Examples

This directory contains example implementations showing how to use KnotifyEx in real-world applications.

## FileWatcherExample

A complete GenServer implementation that demonstrates best practices for using KnotifyEx.

### Features

- **Event Tracking**: Maintains statistics for all file system events (creates, modifies, removes, renames)
- **Recent Files**: Tracks the most recent file changes with timestamps
- **Event Filtering**: Demonstrates how to filter for specific event types
- **Proper Lifecycle Management**: Shows correct initialization and cleanup
- **handle_info Pattern**: Full implementation of GenServer callbacks

### Running the Example

```elixir
# Start an IEx session
iex -S mix

# Compile the example
Code.compile_file("examples/file_watcher_example.ex")

# Create a test directory
File.mkdir_p!("/tmp/watch_test")

# Start the watcher
{:ok, pid} = FileWatcherExample.start_link(watch_dir: "/tmp/watch_test")

# In another terminal, create some files:
# touch /tmp/watch_test/test.txt
# echo "hello" >> /tmp/watch_test/test.txt

# Check the statistics
FileWatcherExample.get_stats(pid)
# => %{creates: 1, modifies: 1, removes: 0, renames: 0}

# Get recent file changes
FileWatcherExample.get_recent_files(pid)
# => [{"/tmp/watch_test/test.txt", [:modify], ~U[2024-01-15 10:30:00.123Z]}, ...]

# Reset stats
FileWatcherExample.reset_stats(pid)

# Stop the watcher
FileWatcherExample.stop(pid)
```

### With Event Filtering

```elixir
# Only watch for create and modify events
{:ok, pid} = FileWatcherExample.start_link(
  watch_dir: "/tmp/watch_test",
  events: [:create, :modify]
)

# Delete events won't be tracked
```

### Running Tests

```bash
# Run all tests including the example tests
mix test

# Run only the example tests
mix test test/file_watcher_example_test.exs

# Run only integration tests
mix test test/file_watcher_example_test.exs --only integration
```

## Key Patterns Demonstrated

### 1. GenServer with KnotifyEx

```elixir
defmodule MyWatcher do
  use GenServer

  def init(opts) do
    {:ok, watcher_pid} = KnotifyEx.start_link(dirs: ["/path/to/watch"])
    KnotifyEx.subscribe(watcher_pid)

    {:ok, %{watcher_pid: watcher_pid}}
  end

  # Handle file events
  def handle_info({:file_event, watcher_pid, {path, events}}, state) do
    # Process the file event
    {:noreply, state}
  end

  # Handle watcher stopped
  def handle_info({:file_event, watcher_pid, :stop}, state) do
    {:stop, :watcher_stopped, state}
  end

  # Clean up on termination
  def terminate(_reason, state) do
    if state.watcher_pid && Process.alive?(state.watcher_pid) do
      KnotifyEx.stop(state.watcher_pid)
    end
    :ok
  end
end
```

### 2. Event Filtering

```elixir
# Subscribe to specific event types
KnotifyEx.subscribe(watcher_pid, events: [:create, :modify])

# Or filter in your handle_info
def handle_info({:file_event, _, {path, events}}, state) do
  if :create in events do
    handle_new_file(path)
  end
  {:noreply, state}
end
```

### 3. Pattern Matching on Events

```elixir
def handle_info({:file_event, _, {path, events}}, state) do
  cond do
    :create in events -> handle_create(path)
    :modify in events -> handle_modify(path)
    :remove in events -> handle_remove(path)
    :rename in events -> handle_rename(path)
  end
  {:noreply, state}
end
```

### 4. Tracking State

```elixir
def handle_info({:file_event, _, {path, events}}, state) do
  new_state =
    state
    |> update_statistics(events)
    |> add_to_recent_files(path, events)
    |> maybe_process_file(path, events)

  {:noreply, new_state}
end
```

## Common Use Cases

### Configuration File Watcher

```elixir
def handle_info({:file_event, _, {path, events}}, state) do
  if Path.basename(path) == "config.json" and :modify in events do
    new_config = load_config(path)
    {:noreply, %{state | config: new_config}}
  else
    {:noreply, state}
  end
end
```

### Hot Reload Development

```elixir
def handle_info({:file_event, _, {path, events}}, state) do
  if Path.extname(path) == ".ex" and :modify in events do
    Code.compile_file(path)
    Logger.info("Reloaded: #{path}")
  end
  {:noreply, state}
end
```

### File Processing Pipeline

```elixir
def handle_info({:file_event, _, {path, events}}, state) do
  if :create in events and Path.extname(path) == ".csv" do
    Task.start(fn -> process_csv_file(path) end)
  end
  {:noreply, state}
end
```

### Log File Monitor

```elixir
def handle_info({:file_event, _, {path, events}}, state) do
  if :modify in events and String.ends_with?(path, ".log") do
    new_lines = read_new_lines(path, state.last_position)
    analyze_logs(new_lines)
    {:noreply, update_position(state, path)}
  else
    {:noreply, state}
  end
end
```

## Tips and Best Practices

1. **Always handle :stop events** - The watcher can stop unexpectedly
2. **Use debouncing** - Set an appropriate debounce value to avoid duplicate events
3. **Filter early** - Use subscription filters or early pattern matching to avoid processing unwanted events
4. **Handle errors gracefully** - File operations can fail, always use defensive coding
5. **Consider async processing** - Use Tasks for long-running file operations
6. **Clean up resources** - Always stop the watcher in your terminate callback
7. **Monitor the watcher** - Consider linking or monitoring the watcher process
8. **Test with integration tests** - File system operations are best tested with actual files

## Performance Considerations

- Use event filtering to reduce the number of messages your process receives
- Consider using debouncing to group rapid changes
- For large directories, be aware of the initial scan time
- Process file events asynchronously if they involve heavy I/O
- Keep track of position when watching log files to avoid re-reading

## Troubleshooting

### Events not being received

1. Check that the directory exists and is accessible
2. Verify the watcher process is still alive
3. Ensure you've subscribed to the watcher
4. Check your event filters aren't too restrictive
5. Increase the timeout if using `assert_receive` in tests

### Too many events

1. Increase the debounce value
2. Use event filtering to reduce message volume
3. Consider recursive: false if you don't need subdirectories
4. Filter by file extension or pattern in your handle_info

### Performance issues

1. Reduce the number of directories being watched
2. Process events asynchronously with Tasks
3. Use event filtering at subscription time
4. Consider batching operations
