defmodule JsonlPort do
  @moduledoc """
  Runs a program that speaks the beamports `--jsonl` request protocol as an
  Erlang port, and sends it one request at a time.

  The protocol is one JSON object per line in each direction: a request
  `{"version":1,"request_id":N,"operation":"...","payload":{...}}` and a
  reply with the same `request_id` and either `"ok":true,"data":...` or
  `"ok":false,"error":"..."`. duckport, oxiport and tantiport speak it when
  started with `--jsonl`.

  Each `JsonlPort` process owns one program, started on the first request
  and restarted on the next request after it exits. Requests are serialised:
  a request waits until the one before it has been answered.

  A request that gets no valid reply in time ends with an error, and the
  program is killed before the next request starts. A write that the
  program finishes later therefore cannot overlap the next request, but it
  may still have happened: the caller has to treat
  `{:error, :uncertain_native_outcome}` as unknown, not as failed.

      {:ok, pid} =
        JsonlPort.start_link(
          executable: "/usr/local/bin/duckport",
          args: ["--jsonl", "--database", "/var/db/app/analytics.duckdb"]
        )

      {:ok, %{"rows" => [%{"answer" => 42}]}} =
        JsonlPort.request(pid, "query", %{sql: "SELECT 42 AS answer", params: []})
  """
  use GenServer

  @max_packet 16 * 1024 * 1024

  @typedoc """
  Why a request failed.

    * `:missing_executable` - `:executable` is not a regular file.
    * `:packet_limit` - the request or the reply is larger than 16 MiB.
    * `:invalid_protocol` - the reply is not JSON, has another version, or
      answers another request.
    * `:uncertain_native_outcome` - no reply before the timeout; the
      program was killed, and the request may or may not have taken effect.
    * `{:native_exit, status}` - the program exited before replying.
    * a binary - the program's own error, from `"ok":false`; or the message
      of an exception raised while handling the request.
  """
  @type error ::
          :missing_executable
          | :packet_limit
          | :invalid_protocol
          | :uncertain_native_outcome
          | {:native_exit, term()}
          | String.t()

  @doc """
  Starts a process for one program. The program itself starts with the
  first request.

  Options:

    * `:executable` - absolute path of the program (required).
    * `:args` - its arguments, for example `["--jsonl", "--index", path]`.
    * `:name` - registers the process, as in `GenServer.start_link/3`.
  """
  @spec start_link(keyword()) :: GenServer.on_start()
  def start_link(opts), do: GenServer.start_link(__MODULE__, opts, name: opts[:name])

  @doc """
  Sends one request and waits up to `timeout` milliseconds for its reply.

  `payload` is encoded with Jason. Returns `{:ok, data}` with the reply's
  decoded `data`, or `{:error, reason}` (see `t:error/0`).
  """
  @spec request(GenServer.server(), String.t(), term(), timeout()) ::
          {:ok, term()} | {:error, error()}
  def request(server, operation, payload \\ %{}, timeout \\ 30_000),
    do: GenServer.call(server, {:request, operation, payload, timeout}, timeout + 5_000)

  @doc """
  Kills the program, if it is running, once the request in progress (if
  any) has been answered. The next request starts it again.
  """
  @spec reset(GenServer.server(), timeout()) :: :ok
  def reset(server, timeout \\ 5_000), do: GenServer.call(server, :reset, timeout)

  @impl true
  def init(opts) do
    Process.flag(:trap_exit, true)
    {:ok, %{opts: opts, port: nil, seq: 0}}
  end

  @impl true
  def handle_call(:reset, _, state) do
    if state.port, do: stop(state.port)
    {:reply, :ok, %{state | port: nil}}
  end

  @impl true
  def handle_call({:request, operation, payload, timeout}, _from, state) do
    case connect(state) do
      {:ok, state} ->
        id = state.seq + 1

        request =
          Jason.encode!(%{version: 1, request_id: id, operation: operation, payload: payload})

        if byte_size(request) > @max_packet do
          {:reply, {:error, :packet_limit}, state}
        else
          Port.command(state.port, request <> "\n")
          result = read(state.port, id, System.monotonic_time(:millisecond) + timeout, "")

          case result do
            {:ok, _} ->
              {:reply, result, %{state | seq: id}}

            {:remote_error, error} ->
              {:reply, {:error, error}, %{state | seq: id}}

            {:error, _} ->
              stop(state.port)
              {:reply, result, %{state | port: nil, seq: id}}
          end
        end

      {:error, error} ->
        {:reply, {:error, error}, state}
    end
  rescue
    e ->
      if state.port, do: stop(state.port)
      {:reply, {:error, Exception.message(e)}, %{state | port: nil}}
  end

  @impl true
  def handle_info({port, {:exit_status, _}}, %{port: port} = state),
    do: {:noreply, %{state | port: nil}}

  def handle_info({:EXIT, port, _}, %{port: port} = state), do: {:noreply, %{state | port: nil}}
  def handle_info(_, state), do: {:noreply, state}

  @impl true
  def terminate(_, state), do: if(state.port, do: stop(state.port))

  defp connect(%{port: port} = state) when not is_nil(port) do
    if Port.info(port), do: {:ok, state}, else: connect(%{state | port: nil})
  end

  defp connect(state) do
    executable = state.opts[:executable]

    if is_binary(executable) and File.regular?(executable) do
      port =
        Port.open({:spawn_executable, executable}, [
          :binary,
          :exit_status,
          {:line, 65_536},
          args: state.opts[:args] || []
        ])

      {:ok, %{state | port: port}}
    else
      {:error, :missing_executable}
    end
  end

  defp read(port, id, deadline, buffer) do
    remaining = max(deadline - System.monotonic_time(:millisecond), 0)

    receive do
      {^port, {:data, {kind, bytes}}} ->
        buffer = buffer <> bytes

        cond do
          byte_size(buffer) > @max_packet ->
            {:error, :packet_limit}

          kind == :noeol ->
            read(port, id, deadline, buffer)

          true ->
            case Jason.decode(buffer) do
              {:ok, %{"version" => 1, "request_id" => ^id, "ok" => true, "data" => data}} ->
                {:ok, data}

              {:ok, %{"version" => 1, "request_id" => ^id, "ok" => false, "error" => error}} ->
                {:remote_error, error}

              _ ->
                {:error, :invalid_protocol}
            end
        end

      {^port, {:exit_status, code}} ->
        {:error, {:native_exit, code}}

      {:EXIT, ^port, reason} ->
        {:error, {:native_exit, reason}}
    after
      remaining -> {:error, :uncertain_native_outcome}
    end
  end

  # Kill the actual native process before releasing the serialised caller.
  # A closed stdin or an abandoned request must never leave a late write
  # running.
  defp stop(port) do
    case Port.info(port, :os_pid) do
      {:os_pid, pid} ->
        if match?({:win32, _}, :os.type()) do
          System.cmd("taskkill", ["/PID", to_string(pid), "/T", "/F"], stderr_to_stdout: true)
        else
          System.cmd("kill", ["-KILL", to_string(pid)], stderr_to_stdout: true)
        end

        await_exit(pid, System.monotonic_time(:millisecond) + 5_000)

      _ ->
        :ok
    end

    if Port.info(port), do: Port.close(port)
    :ok
  end

  defp await_exit(pid, deadline) do
    alive =
      if match?({:win32, _}, :os.type()) do
        {output, _} =
          System.cmd("tasklist", ["/FI", "PID eq #{pid}", "/FO", "CSV", "/NH"],
            stderr_to_stdout: true
          )

        String.contains?(output, "\",\"#{pid}\",")
      else
        {_, code} = System.cmd("kill", ["-0", to_string(pid)], stderr_to_stdout: true)
        code == 0
      end

    if alive do
      if System.monotonic_time(:millisecond) >= deadline, do: raise("native process did not exit")
      Process.sleep(25)
      await_exit(pid, deadline)
    end
  end
end
