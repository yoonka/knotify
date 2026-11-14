defmodule KnotifyEx.Worker do
  @moduledoc """
  Internal worker process that manages the knotify binary and handles file system events.

  This module is not part of the public API. You should use the `KnotifyEx` module instead.

  The worker:
    - Spawns and manages the knotify binary as a port
    - Parses JSON events from knotify
    - Manages subscriber processes and event filtering
    - Handles automatic cleanup when subscribers die
  """
  use GenServer
  require Logger

  defstruct [
    :port,
    :dirs,
    :recursive,
    :backend,
    :debounce,
    :executable,
    protocol: :json,
    ready: nil,
    primary: nil,
    granted: false,
    last_batch: -1,
    pending: %{},
    subscribers: %{}
  ]

  @valid_events [:create, :create_folder, :modify, :remove, :remove_folder, :rename]

  # Mapping from knotify event kinds to our events
  @event_mapping %{
    "CREATE_FILE" => :create,
    "CREATE_FOLDER" => :create_folder,
    "CREATE" => :create,
    "MODIFY_CONTENT" => :modify,
    "MODIFY_SIZE" => :modify,
    "MODIFY_DATA" => :modify,
    "MODIFY" => :modify,
    "RENAME_FROM" => :rename,
    "RENAME_TO" => :rename,
    "RENAME_BOTH" => :rename,
    "RENAME" => :rename,
    "REMOVE_FILE" => :remove,
    "REMOVE_FOLDER" => :remove_folder,
    "REMOVE" => :remove
  }

  ## Public API

  @doc false
  def validate_subscription!(opts) do
    get_event_filter(opts)
    :ok
  end

  def start_link(opts, genserver_opts \\ []) do
    GenServer.start_link(__MODULE__, opts, genserver_opts)
  end

  ## GenServer Callbacks

  @impl true
  def init(opts) do
    dirs = Keyword.fetch!(opts, :dirs)
    recursive = Keyword.get(opts, :recursive, true)
    backend = Keyword.get(opts, :backend, :auto)
    debounce = Keyword.get(opts, :debounce, 100)
    protocol = Keyword.get(opts, :protocol, :json)

    unless protocol in [:json, :etf], do: raise(ArgumentError, "protocol must be :json or :etf")

    if protocol == :etf and length(dirs) != 1,
      do: raise(ArgumentError, "ETF hint mode requires exactly one root")

    # Validate directories exist
    case validate_dirs(dirs) do
      :ok ->
        state = %__MODULE__{
          dirs: dirs,
          recursive: recursive,
          backend: backend,
          debounce: debounce,
          protocol: protocol,
          executable: Keyword.get(opts, :executable),
          subscribers: %{}
        }

        {:ok, state, {:continue, :start_watcher}}

      {:error, reason} ->
        {:stop, reason}
    end
  end

  @impl true
  def handle_continue(:start_watcher, state) do
    case start_port(state) do
      {:ok, port} ->
        {:noreply, %{state | port: port}}

      {:error, reason} ->
        {:stop, reason, state}
    end
  end

  @impl true
  def handle_call({:subscribe, pid, opts}, _from, state) do
    if state.protocol == :etf and Keyword.has_key?(opts, :events) do
      {:reply, {:error, :filters_not_supported_in_hint_mode}, state}
    else
      if state.protocol == :etf and state.primary not in [nil, pid] do
        {:reply, {:error, :hint_collector_already_subscribed}, state}
      else
        subscribe(pid, opts, state)
      end
    end
  end

  def handle_call({:ack, batch_id}, {pid, _}, %{protocol: :etf} = state) do
    if state.primary == pid and Map.has_key?(state.pending, batch_id) do
      :ok = credit(state.port, 1)
      {:reply, :ok, %{state | pending: Map.delete(state.pending, batch_id)}}
    else
      {:reply, {:error, :invalid_acknowledgement}, state}
    end
  end

  def handle_call({:ack, _batch_id}, _from, state),
    do: {:reply, {:error, :not_hint_protocol}, state}

  def handle_call({:unsubscribe, pid}, _from, %{protocol: :etf, primary: pid} = state),
    do: {:stop, :normal, :ok, state}

  def handle_call({:unsubscribe, pid}, _from, state) do
    state = remove_subscriber(state, pid)
    {:reply, :ok, state}
  end

  def handle_call(:known_dirs, _from, state), do: {:reply, state.dirs, state}

  defp subscribe(pid, opts, state) do
    event_filter = get_event_filter(opts)
    state = remove_subscriber(state, pid)

    # Monitor the subscriber so we can clean up if it dies
    ref = Process.monitor(pid)

    subscribers =
      Map.put(state.subscribers, pid, %{
        ref: ref,
        events: event_filter
      })

    state = %{state | subscribers: subscribers}
    state = if state.protocol == :etf, do: %{state | primary: pid}, else: state
    if state.ready, do: send(pid, {:knotify, self(), state.ready})
    {:reply, :ok, grant_initial_credit(state)}
  end

  @impl true
  def handle_info({port, {:data, packet}}, %{port: port, protocol: :etf} = state)
      when is_binary(packet) do
    case decode_hint(packet) do
      {:ok, %{"type" => "ready"} = message} ->
        if state.ready do
          protocol_error(state, :duplicate_ready)
        else
          if state.primary, do: send(state.primary, {:knotify, self(), message})
          {:noreply, grant_initial_credit(%{state | ready: message})}
        end

      {:ok, %{"batch_id" => id} = message} ->
        if state.primary && state.ready && id > state.last_batch && map_size(state.pending) < 4 do
          send(state.primary, {:knotify, self(), message})
          {:noreply, %{state | last_batch: id, pending: Map.put(state.pending, id, true)}}
        else
          protocol_error(state, :invalid_batch)
        end

      {:ok, %{"type" => "error"} = message} ->
        if state.primary, do: send(state.primary, {:knotify, self(), message})
        {:noreply, state}

      {:error, reason} ->
        protocol_error(state, reason)
    end
  end

  def handle_info({port, {:data, {:eol, line}}}, %{port: port} = state) do
    case decode_event(line) do
      {:ok, paths, events} ->
        # knotify can return multiple paths per event (e.g., rename operations)
        # We'll broadcast each path separately for simplicity
        Enum.each(paths, fn path ->
          broadcast_event(state, {path, events})
        end)

        {:noreply, state}

      {:error, reason} ->
        Logger.warning("Failed to decode event: #{inspect(reason)}")
        {:noreply, state}
    end
  end

  @impl true
  def handle_info({port, {:exit_status, status}}, %{port: port} = state) do
    Logger.error("knotify binary exited with status: #{status}")
    broadcast_event(state, :stop)
    {:stop, {:port_exit, status}, state}
  end

  @impl true
  def handle_info({:DOWN, _ref, :process, pid, _reason}, state) do
    if state.protocol == :etf and state.primary == pid,
      do: {:stop, :normal, remove_subscriber(state, pid)},
      else: {:noreply, remove_subscriber(state, pid)}
  end

  @impl true
  def handle_info(msg, state) do
    Logger.debug("Unexpected message: #{inspect(msg)}")
    {:noreply, state}
  end

  @impl true
  def terminate(_reason, state) do
    if state.port do
      if Port.info(state.port), do: Port.close(state.port)
    end

    broadcast_event(state, :stop)
    :ok
  end

  ## Private Functions

  defp validate_dirs(dirs) when is_list(dirs) do
    invalid_dirs =
      Enum.reject(dirs, fn dir ->
        File.dir?(dir)
      end)

    case invalid_dirs do
      [] ->
        :ok

      dirs ->
        {:error, {:invalid_directories, dirs}}
    end
  end

  defp validate_dirs(_), do: {:error, :dirs_must_be_list}

  defp start_port(state) do
    executable = state.executable || find_executable()
    args = build_args(state)
    framing = if state.protocol == :etf, do: [{:packet, 4}], else: [{:line, 1024 * 1024}]

    port =
      Port.open(
        {:spawn_executable, executable},
        [
          :binary,
          :exit_status,
          env: [{~c"KNOTIFY_EXIT_ON_EOF", ~c"1"}],
          args: args
        ] ++ framing
      )

    {:ok, port}
  rescue
    e ->
      {:error, {:port_error, e}}
  end

  defp find_executable do
    # Find knotify binary in system PATH
    System.find_executable("knotify") ||
      raise "Could not find knotify binary in system PATH. Please ensure knotify is installed."
  end

  defp build_args(state) do
    base_args = [
      "--json",
      "--debounce",
      to_string(state.debounce),
      "--backend",
      to_string(state.backend)
    ]

    recursive_args =
      if state.recursive do
        ["--recursive"]
      else
        []
      end

    dir_args =
      Enum.flat_map(state.dirs, fn dir ->
        ["--path", dir]
      end)

    protocol_args = if state.protocol == :etf, do: ["--protocol", "etf"], else: []
    base_args ++ recursive_args ++ dir_args ++ protocol_args
  end

  defp decode_event(data) do
    # Expected JSON format from knotify:
    # {"kind": "CREATE_FILE", "paths": ["/path/to/file"]}
    case Jason.decode(data) do
      {:ok, %{"kind" => kind, "paths" => paths}} when is_list(paths) ->
        case Map.get(@event_mapping, kind) do
          nil ->
            Logger.debug("Unknown event kind: #{kind}")
            {:error, {:unknown_event_kind, kind}}

          event ->
            {:ok, paths, [event]}
        end

      {:ok, invalid} ->
        Logger.warning("Invalid event format: #{inspect(invalid)}")
        {:error, :invalid_format}

      {:error, reason} ->
        {:error, {:json_decode, reason}}
    end
  end

  defp get_event_filter(opts) do
    case Keyword.get(opts, :events) do
      nil ->
        :all

      events when is_list(events) ->
        invalid_events = Enum.reject(events, &(&1 in @valid_events))

        if invalid_events != [] do
          raise ArgumentError,
                "Invalid events: #{inspect(invalid_events)}. " <>
                  "Valid events are: #{inspect(@valid_events)}"
        end

        MapSet.new(events)
    end
  end

  defp broadcast_event(state, event) do
    Enum.each(state.subscribers, fn {pid, subscriber_info} ->
      if state.protocol == :etf do
        if event == :stop, do: send(pid, {:knotify, self(), %{"version" => 1, "type" => "stop"}})
      else
        if should_send_event?(event, subscriber_info.events) do
          send(pid, {:file_event, self(), event})
        end
      end
    end)
  end

  defp grant_initial_credit(%{protocol: :etf, ready: ready, primary: pid, granted: false} = state)
       when not is_nil(ready) and is_pid(pid) do
    :ok = credit(state.port, 4)
    %{state | granted: true}
  end

  defp grant_initial_credit(state), do: state

  defp credit(port, count) do
    true =
      Port.command(
        port,
        :erlang.term_to_binary(%{"version" => 1, "type" => "credit", "count" => count})
      )

    :ok
  end

  defp protocol_error(state, reason) do
    if state.primary do
      send(
        state.primary,
        {:knotify, self(),
         %{
           "version" => 1,
           "type" => "error",
           "code" => "protocol_error",
           "message" => inspect(reason)
         }}
      )
    end

    {:stop, {:protocol_error, reason}, state}
  end

  defp decode_hint(<<131, 80, _::binary>>), do: {:error, :compressed_etf_not_supported}

  defp decode_hint(packet) when byte_size(packet) <= 1_048_576 do
    try do
      case :erlang.binary_to_term(packet, [:safe]) do
        %{"version" => 1, "type" => "ready", "service" => "watcher"} = message ->
          {:ok, message}

        %{"version" => 1, "type" => "paths", "batch_id" => id, "paths" => paths} = message
        when is_integer(id) and id >= 0 and is_list(paths) and length(paths) <= 128 ->
          if Enum.all?(paths, &(is_binary(&1) and byte_size(&1) <= 131_072)),
            do: {:ok, message},
            else: {:error, :invalid_paths}

        %{"version" => 1, "type" => "rescan", "batch_id" => id, "reason" => reason} = message
        when is_integer(id) and id >= 0 and is_binary(reason) ->
          case Map.get(message, "paths", []) do
            paths when is_list(paths) and length(paths) <= 128 ->
              if Enum.all?(paths, &(is_binary(&1) and byte_size(&1) <= 131_072)),
                do: {:ok, message},
                else: {:error, :invalid_rescan_paths}

            _ ->
              {:error, :invalid_rescan_paths}
          end

        %{"version" => 1, "type" => "error", "code" => code, "message" => text} = message
        when is_binary(code) and is_binary(text) ->
          {:ok, message}

        _ ->
          {:error, :incompatible_message}
      end
    rescue
      ArgumentError -> {:error, :invalid_etf}
    end
  end

  defp decode_hint(_), do: {:error, :packet_too_large}

  defp should_send_event?(:stop, _filter), do: true
  defp should_send_event?(_event, :all), do: true

  defp should_send_event?({_path, events}, filter) when is_map(filter) do
    Enum.any?(events, fn event ->
      MapSet.member?(filter, event)
    end)
  end

  defp remove_subscriber(state, pid) do
    case Map.get(state.subscribers, pid) do
      nil ->
        state

      %{ref: ref} ->
        Process.demonitor(ref, [:flush])
        %{state | subscribers: Map.delete(state.subscribers, pid)}
    end
  end
end
