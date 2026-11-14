defmodule JsonlPortTest do
  use ExUnit.Case, async: false

  setup do
    script = """
    const fs = require('fs');
    const rl = require('readline').createInterface({input: process.stdin});
    rl.on('line', line => {
      const r = JSON.parse(line);
      const send = id => process.stdout.write(JSON.stringify({version:1,request_id:id,ok:true,data:{protocol:1,durable:true}})+'\\n');
      if (r.operation === 'sleep') {
        if (r.payload.started) fs.writeFileSync(r.payload.started, 'started');
        setTimeout(() => {fs.writeFileSync(r.payload.path, 'late write');send(r.request_id);}, 700);
      }
      else if (r.operation === 'fail') process.stdout.write(JSON.stringify({version:1,request_id:r.request_id,ok:false,error:'missing_sql'})+'\\n');
      else send(r.operation === 'bad' ? r.request_id + 1 : r.request_id);
    });
    """

    node = System.find_executable("node")
    assert node
    pid = start_supervised!({JsonlPort, executable: node, args: ["-e", script]})
    %{pid: pid}
  end

  test "an uncertain timeout kills the old native process before another request", %{pid: pid} do
    path =
      Path.join(
        System.tmp_dir!(),
        "jsonl-port-no-late-write-#{Base.encode16(:crypto.strong_rand_bytes(8))}"
      )

    assert {:ok, _} = JsonlPort.request(pid, "health")

    assert {:error, :uncertain_native_outcome} =
             JsonlPort.request(pid, "sleep", %{path: path}, 50)

    assert {:ok, _} = JsonlPort.request(pid, "health")
    Process.sleep(900)
    refute File.exists?(path)
  end

  test "the program's own error is returned and the program keeps running", %{pid: pid} do
    assert {:error, "missing_sql"} = JsonlPort.request(pid, "fail")
    native_port = :sys.get_state(pid).port
    assert {:ok, _} = JsonlPort.request(pid, "health")
    assert :sys.get_state(pid).port == native_port
  end

  test "a missing executable is reported without starting anything" do
    pid =
      start_supervised!(
        Supervisor.child_spec({JsonlPort, executable: "/nonexistent/program"}, id: :missing)
      )

    assert {:error, :missing_executable} = JsonlPort.request(pid, "health")
  end

  test "mismatched correlation is rejected and the next connection is clean", %{pid: pid} do
    assert {:error, :invalid_protocol} = JsonlPort.request(pid, "bad")
    assert {:ok, _} = JsonlPort.request(pid, "health")
  end

  test "a recovery reset waits behind an in-flight write and closes its process", %{pid: pid} do
    path =
      Path.join(
        System.tmp_dir!(),
        "jsonl-port-reset-barrier-#{System.unique_integer([:positive])}"
      )

    started = path <> ".started"
    on_exit(fn -> Enum.each([path, started], &File.rm/1) end)
    assert {:ok, _} = JsonlPort.request(pid, "health")
    native_port = :sys.get_state(pid).port

    task =
      Task.async(fn ->
        JsonlPort.request(pid, "sleep", %{path: path, started: started}, 2_000)
      end)

    assert Enum.reduce_while(1..100, false, fn _, _ ->
             if File.exists?(started) do
               {:halt, true}
             else
               Process.sleep(10)
               {:cont, false}
             end
           end)

    assert :ok = JsonlPort.reset(pid, :infinity)
    assert {:ok, _} = Task.await(task)
    assert File.read!(path) == "late write"
    assert Port.info(native_port) == nil
    assert {:ok, _} = JsonlPort.request(pid, "health")
  end
end
