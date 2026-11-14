defmodule KnotifyEx.WorkerTest do
  use ExUnit.Case, async: false
  import ExUnit.CaptureLog

  alias KnotifyEx.Worker

  @test_dir Path.join(
              Path.expand(System.tmp_dir!()),
              "knotify_worker_test_#{System.unique_integer([:positive])}"
            )

  setup do
    # Create test directory
    File.mkdir_p!(@test_dir)

    on_exit(fn ->
      # Cleanup test directory
      File.rm_rf!(@test_dir)
    end)

    :ok
  end

  describe "init/1" do
    test "initializes with valid options" do
      opts = [dirs: [@test_dir], recursive: true, backend: :auto, debounce: 100]
      assert {:ok, state, {:continue, :start_watcher}} = Worker.init(opts)
      assert state.dirs == [@test_dir]
      assert state.recursive == true
      assert state.backend == :auto
      assert state.debounce == 100
      assert state.subscribers == %{}
    end

    test "uses default values for optional parameters" do
      opts = [dirs: [@test_dir]]
      assert {:ok, state, {:continue, :start_watcher}} = Worker.init(opts)
      assert state.recursive == true
      assert state.backend == :auto
      assert state.debounce == 100
    end

    test "returns error for missing dirs option" do
      assert_raise KeyError, fn ->
        Worker.init([])
      end
    end

    test "returns error for non-existent directories" do
      opts = [dirs: ["/nonexistent/path"]]
      assert {:stop, {:invalid_directories, ["/nonexistent/path"]}} = Worker.init(opts)
    end

    test "returns error when dirs is not a list" do
      opts = [dirs: "not_a_list"]
      assert {:stop, :dirs_must_be_list} = Worker.init(opts)
    end

    test "validates all directories exist" do
      valid_dir = @test_dir
      invalid_dir = "/nonexistent/path"
      opts = [dirs: [valid_dir, invalid_dir]]

      assert {:stop, {:invalid_directories, [^invalid_dir]}} = Worker.init(opts)
    end
  end

  describe "handle_call {:subscribe, pid, opts}" do
    setup do
      {:ok, pid} = Worker.start_link(dirs: [@test_dir])
      state = :sys.get_state(pid)
      %{worker: pid, state: state}
    end

    test "adds subscriber to state", %{worker: worker} do
      subscriber_pid = self()

      {:reply, :ok, new_state} =
        Worker.handle_call({:subscribe, subscriber_pid, []}, nil, :sys.get_state(worker))

      assert Map.has_key?(new_state.subscribers, subscriber_pid)
      assert new_state.subscribers[subscriber_pid].events == :all
    end

    test "adds subscriber with event filter", %{worker: worker} do
      subscriber_pid = self()

      {:reply, :ok, new_state} =
        Worker.handle_call(
          {:subscribe, subscriber_pid, [events: [:create, :modify]]},
          nil,
          :sys.get_state(worker)
        )

      subscriber_info = new_state.subscribers[subscriber_pid]
      assert MapSet.member?(subscriber_info.events, :create)
      assert MapSet.member?(subscriber_info.events, :modify)
      refute MapSet.member?(subscriber_info.events, :remove)
    end

    test "monitors subscriber process", %{worker: worker} do
      subscriber_pid = spawn(fn -> receive do: (:wait -> :ok) end)

      {:reply, :ok, new_state} =
        Worker.handle_call({:subscribe, subscriber_pid, []}, nil, :sys.get_state(worker))

      subscriber_info = new_state.subscribers[subscriber_pid]
      assert is_reference(subscriber_info.ref)
    end
  end

  describe "handle_call {:unsubscribe, pid}" do
    setup do
      {:ok, worker} = Worker.start_link(dirs: [@test_dir])
      subscriber_pid = self()
      GenServer.call(worker, {:subscribe, subscriber_pid, []})
      %{worker: worker, subscriber: subscriber_pid}
    end

    test "removes subscriber from state", %{worker: worker, subscriber: subscriber} do
      state = :sys.get_state(worker)
      assert Map.has_key?(state.subscribers, subscriber)

      {:reply, :ok, new_state} = Worker.handle_call({:unsubscribe, subscriber}, nil, state)

      refute Map.has_key?(new_state.subscribers, subscriber)
    end

    test "demonitors subscriber process", %{worker: worker, subscriber: subscriber} do
      state = :sys.get_state(worker)
      ref = state.subscribers[subscriber].ref

      {:reply, :ok, _new_state} = Worker.handle_call({:unsubscribe, subscriber}, nil, state)

      # The monitor should be demonitored
      assert Process.info(self(), :monitors) == {:monitors, []}
    end

    test "unsubscribing non-existent subscriber is safe", %{worker: worker} do
      non_existent_pid = spawn(fn -> :ok end)
      state = :sys.get_state(worker)

      {:reply, :ok, new_state} = Worker.handle_call({:unsubscribe, non_existent_pid}, nil, state)

      assert new_state == state
    end
  end

  describe "handle_call :known_dirs" do
    test "returns the list of watched directories" do
      dirs = [@test_dir, "/tmp"]
      {:ok, worker} = Worker.start_link(dirs: dirs)
      state = :sys.get_state(worker)

      {:reply, result, ^state} = Worker.handle_call(:known_dirs, nil, state)
      assert result == dirs
    end
  end

  describe "handle_info {:DOWN, ref, :process, pid, reason}" do
    setup do
      {:ok, worker} = Worker.start_link(dirs: [@test_dir])
      %{worker: worker}
    end

    test "removes dead subscriber from state", %{worker: worker} do
      # Create a subscriber process
      subscriber_pid = spawn(fn -> receive do: (:wait -> :ok) end)
      GenServer.call(worker, {:subscribe, subscriber_pid, []})

      state_before = :sys.get_state(worker)
      assert Map.has_key?(state_before.subscribers, subscriber_pid)
      ref = state_before.subscribers[subscriber_pid].ref

      # Simulate the subscriber process dying
      {:noreply, new_state} =
        Worker.handle_info({:DOWN, ref, :process, subscriber_pid, :normal}, state_before)

      refute Map.has_key?(new_state.subscribers, subscriber_pid)
    end
  end

  describe "handle_info port messages" do
    setup do
      {:ok, worker} = Worker.start_link(dirs: [@test_dir])
      GenServer.call(worker, {:subscribe, self(), []})
      state = :sys.get_state(worker)
      %{worker: worker, state: state}
    end

    test "decodes and broadcasts valid JSON events", %{state: state} do
      json_event = Jason.encode!(%{"kind" => "CREATE_FILE", "paths" => ["/tmp/test.txt"]})
      port = state.port

      {:noreply, _new_state} = Worker.handle_info({port, {:data, {:eol, json_event}}}, state)

      assert_receive {:file_event, _, {"/tmp/test.txt", [:create]}}
    end

    test "handles multiple paths in event", %{state: state} do
      json_event =
        Jason.encode!(%{"kind" => "RENAME", "paths" => ["/tmp/old.txt", "/tmp/new.txt"]})

      port = state.port

      {:noreply, _new_state} = Worker.handle_info({port, {:data, {:eol, json_event}}}, state)

      assert_receive {:file_event, _, {"/tmp/old.txt", [:rename]}}
      assert_receive {:file_event, _, {"/tmp/new.txt", [:rename]}}
    end

    test "logs warning for invalid JSON", %{state: state} do
      port = state.port

      log =
        capture_log(fn ->
          Worker.handle_info({port, {:data, {:eol, "invalid json"}}}, state)
        end)

      assert log =~ "Failed to decode event"
    end

    test "logs warning for unknown event kind", %{state: state} do
      json_event = Jason.encode!(%{"kind" => "UNKNOWN_EVENT", "paths" => ["/tmp/test.txt"]})
      port = state.port

      log =
        capture_log(fn ->
          Worker.handle_info({port, {:data, {:eol, json_event}}}, state)
        end)

      assert log =~ "Unknown event kind"
    end

    test "handles port exit status", %{state: state} do
      port = state.port

      log =
        capture_log(fn ->
          result = Worker.handle_info({port, {:exit_status, 1}}, state)
          assert {:stop, {:port_exit, 1}, ^state} = result
        end)

      assert log =~ "knotify binary exited with status: 1"
    end

    test "broadcasts stop message on port exit", %{state: state} do
      port = state.port

      capture_log(fn ->
        Worker.handle_info({port, {:exit_status, 1}}, state)
      end)

      assert_receive {:file_event, _, :stop}
    end
  end

  describe "event mapping" do
    setup do
      {:ok, worker} = Worker.start_link(dirs: [@test_dir])
      GenServer.call(worker, {:subscribe, self(), []})
      state = :sys.get_state(worker)
      %{worker: worker, state: state}
    end

    test "maps CREATE_FILE event correctly", %{state: state} do
      port = state.port

      for kind <- ["CREATE_FILE", "CREATE"] do
        json_event = Jason.encode!(%{"kind" => kind, "paths" => ["/tmp/test.txt"]})
        Worker.handle_info({port, {:data, {:eol, json_event}}}, state)
        assert_receive {:file_event, _, {_, [:create]}}
      end
    end

    test "maps CREATE_FOLDER event correctly", %{state: state} do
      port = state.port

      json_event = Jason.encode!(%{"kind" => "CREATE_FOLDER", "paths" => ["/tmp/test_folder"]})
      Worker.handle_info({port, {:data, {:eol, json_event}}}, state)
      assert_receive {:file_event, _, {_, [:create_folder]}}
    end

    test "maps MODIFY events correctly", %{state: state} do
      port = state.port

      for kind <- ["MODIFY_CONTENT", "MODIFY_SIZE", "MODIFY_DATA", "MODIFY"] do
        json_event = Jason.encode!(%{"kind" => kind, "paths" => ["/tmp/test.txt"]})
        Worker.handle_info({port, {:data, {:eol, json_event}}}, state)
        assert_receive {:file_event, _, {_, [:modify]}}
      end
    end

    test "maps RENAME events correctly", %{state: state} do
      port = state.port

      for kind <- ["RENAME_FROM", "RENAME_TO", "RENAME_BOTH", "RENAME"] do
        json_event = Jason.encode!(%{"kind" => kind, "paths" => ["/tmp/test.txt"]})
        Worker.handle_info({port, {:data, {:eol, json_event}}}, state)
        assert_receive {:file_event, _, {_, [:rename]}}
      end
    end

    test "maps REMOVE_FILE event correctly", %{state: state} do
      port = state.port

      for kind <- ["REMOVE_FILE", "REMOVE"] do
        json_event = Jason.encode!(%{"kind" => kind, "paths" => ["/tmp/test.txt"]})
        Worker.handle_info({port, {:data, {:eol, json_event}}}, state)
        assert_receive {:file_event, _, {_, [:remove]}}
      end
    end

    test "maps REMOVE_FOLDER event correctly", %{state: state} do
      port = state.port

      json_event = Jason.encode!(%{"kind" => "REMOVE_FOLDER", "paths" => ["/tmp/test_folder"]})
      Worker.handle_info({port, {:data, {:eol, json_event}}}, state)
      assert_receive {:file_event, _, {_, [:remove_folder]}}
    end
  end

  describe "event filtering" do
    setup do
      {:ok, worker} = Worker.start_link(dirs: [@test_dir])
      %{worker: worker}
    end

    test "subscriber with :all filter receives all events", %{worker: worker} do
      GenServer.call(worker, {:subscribe, self(), []})
      state = :sys.get_state(worker)
      port = state.port

      json_event = Jason.encode!(%{"kind" => "CREATE_FILE", "paths" => ["/tmp/test.txt"]})
      Worker.handle_info({port, {:data, {:eol, json_event}}}, state)

      assert_receive {:file_event, _, {_, [:create]}}
    end

    test "subscriber with event filter receives only matching events", %{worker: worker} do
      GenServer.call(worker, {:subscribe, self(), [events: [:modify]]})
      state = :sys.get_state(worker)
      port = state.port

      # Should not receive create event
      create_event = Jason.encode!(%{"kind" => "CREATE_FILE", "paths" => ["/tmp/test.txt"]})
      Worker.handle_info({port, {:data, {:eol, create_event}}}, state)
      refute_receive {:file_event, _, {_, [:create]}}, 100

      # Should receive modify event
      modify_event = Jason.encode!(%{"kind" => "MODIFY_CONTENT", "paths" => ["/tmp/test.txt"]})
      Worker.handle_info({port, {:data, {:eol, modify_event}}}, state)
      assert_receive {:file_event, _, {_, [:modify]}}
    end

    test "multiple subscribers receive events according to their filters", %{worker: worker} do
      subscriber1 = self()
      subscriber2 = spawn(fn -> receive do: ({:file_event, _, _} -> :ok) end)

      GenServer.call(worker, {:subscribe, subscriber1, [events: [:create]]})
      GenServer.call(worker, {:subscribe, subscriber2, [events: [:modify]]})

      state = :sys.get_state(worker)
      port = state.port

      create_event = Jason.encode!(%{"kind" => "CREATE_FILE", "paths" => ["/tmp/test.txt"]})
      Worker.handle_info({port, {:data, {:eol, create_event}}}, state)

      assert_receive {:file_event, _, {_, [:create]}}
    end
  end

  describe "terminate/2" do
    test "closes port and broadcasts stop message" do
      {:ok, worker} = Worker.start_link(dirs: [@test_dir])
      GenServer.call(worker, {:subscribe, self(), []})

      state = :sys.get_state(worker)
      port = state.port

      assert :ok = Worker.terminate(:normal, state)
      assert_receive {:file_event, _, :stop}

      # Port should be closed (though we can't directly test this)
    end

    test "handles nil port gracefully" do
      state = %Worker{
        dirs: [@test_dir],
        recursive: true,
        backend: :auto,
        debounce: 100,
        subscribers: %{},
        port: nil
      }

      assert :ok = Worker.terminate(:normal, state)
    end
  end
end
