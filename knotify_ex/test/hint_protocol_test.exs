defmodule KnotifyEx.HintProtocolTest do
  use ExUnit.Case, async: false
  @executable System.get_env("KNOTIFY_TEST_EXECUTABLE")
  @moduletag skip: is_nil(@executable)

  setup do
    root = Path.join(System.tmp_dir!(), "knotify-hints-#{System.unique_integer([:positive])}")
    File.mkdir_p!(root)

    {:ok, watcher} =
      KnotifyEx.start_link(dirs: [root], executable: @executable, protocol: :etf, debounce: 10)

    on_exit(fn ->
      if Process.alive?(watcher), do: GenServer.stop(watcher, :normal)
      File.rm_rf!(root)
    end)

    %{root: root, watcher: watcher}
  end

  test "replays readiness to a late subscriber and preserves exact relative names", %{
    root: root,
    watcher: watcher
  } do
    Process.sleep(150)
    assert :ok = KnotifyEx.subscribe(watcher)
    assert_receive {:knotify, ^watcher, %{"type" => "ready", "version" => 1}}, 5_000
    name = "quoted \" café.txt"
    # Double quotes are not valid Windows filename characters.
    name = if match?({:win32, _}, :os.type()), do: "café.txt", else: name
    File.write!(Path.join(root, name), "one")
    assert path_received?(watcher, name)
    File.rm!(Path.join(root, name))
    File.write!(Path.join(root, name), "two")
    assert path_received?(watcher, name)
    assert {:error, :invalid_acknowledgement} = KnotifyEx.ack(watcher, 999_999)
  end

  test "limits outstanding batches and rejects a second collector", %{
    root: root,
    watcher: watcher
  } do
    :ok = KnotifyEx.subscribe(watcher)
    assert_receive {:knotify, ^watcher, %{"type" => "ready"}}, 5_000
    task = Task.async(fn -> KnotifyEx.subscribe(watcher) end)
    assert {:error, :hint_collector_already_subscribed} = Task.await(task)
    for i <- 1..1_000, do: File.write!(Path.join(root, "file-#{i}"), "x")

    batches =
      for _ <- 1..4 do
        assert_receive {:knotify, ^watcher, %{"batch_id" => id}}, 5_000
        id
      end

    refute_receive {:knotify, ^watcher, %{"batch_id" => _}}, 200
    :ok = KnotifyEx.ack(watcher, hd(batches))
    assert_receive {:knotify, ^watcher, %{"batch_id" => _}}, 5_000
  end

  test "an incompatible native message fails with a structured error", %{watcher: watcher} do
    :ok = KnotifyEx.subscribe(watcher)
    assert_receive {:knotify, ^watcher, %{"type" => "ready"}}, 5_000
    Process.unlink(watcher)
    monitor = Process.monitor(watcher)
    port = :sys.get_state(watcher).port
    send(watcher, {port, {:data, :erlang.term_to_binary(%{"version" => 2, "type" => "ready"})}})
    assert_receive {:knotify, ^watcher, %{"type" => "error", "code" => "protocol_error"}}, 5_000
    assert_receive {:DOWN, ^monitor, :process, ^watcher, {:protocol_error, _}}, 5_000
  end

  test "scoped watch errors retain paths and participate in acknowledged delivery", %{
    watcher: watcher
  } do
    :ok = KnotifyEx.subscribe(watcher)
    assert_receive {:knotify, ^watcher, %{"type" => "ready"}}, 5_000
    port = :sys.get_state(watcher).port

    message = %{
      "version" => 1,
      "type" => "rescan",
      "batch_id" => 0,
      "reason" => "watch unavailable",
      "paths" => [<<255, 47, 254>>]
    }

    send(watcher, {port, {:data, :erlang.term_to_binary(message)}})
    assert_receive {:knotify, ^watcher, ^message}, 5_000
    assert :ok = KnotifyEx.ack(watcher, 0)
  end

  test "malformed scoped error paths produce a structured protocol failure", %{watcher: watcher} do
    :ok = KnotifyEx.subscribe(watcher)
    assert_receive {:knotify, ^watcher, %{"type" => "ready"}}, 5_000
    Process.unlink(watcher)
    monitor = Process.monitor(watcher)
    port = :sys.get_state(watcher).port

    send(
      watcher,
      {port,
       {:data,
        :erlang.term_to_binary(%{
          "version" => 1,
          "type" => "rescan",
          "batch_id" => 0,
          "reason" => "error",
          "paths" => [123]
        })}}
    )

    assert_receive {:knotify, ^watcher, %{"type" => "error", "code" => "protocol_error"}}, 5_000

    assert_receive {:DOWN, ^monitor, :process, ^watcher,
                    {:protocol_error, :invalid_rescan_paths}},
                   5_000
  end

  defp path_received?(watcher, wanted, attempts \\ 20)
  defp path_received?(_, _, 0), do: false

  defp path_received?(watcher, wanted, attempts) do
    receive do
      {:knotify, ^watcher, %{"type" => "paths", "batch_id" => id, "paths" => paths}} ->
        :ok = KnotifyEx.ack(watcher, id)
        wanted in paths or path_received?(watcher, wanted, attempts - 1)

      {:knotify, ^watcher, %{"type" => "rescan", "batch_id" => id}} ->
        :ok = KnotifyEx.ack(watcher, id)
        path_received?(watcher, wanted, attempts - 1)
    after
      5_000 -> false
    end
  end
end
