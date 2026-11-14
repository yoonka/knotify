# JsonlPort

Runs a program that speaks the beamports
[`--jsonl` request protocol](https://github.com/yoonka/beamports#the---jsonl-protocol)
as an Erlang port, and sends it one request at a time. duckport, oxiport
and tantiport speak it when started with `--jsonl`, and so can any program
that reads one JSON request per line and writes one JSON reply per line.

## Installation

```elixir
def deps do
  [
    {:jsonl_port, "~> 1.1"}
  ]
end
```

## Usage

```elixir
{:ok, pid} =
  JsonlPort.start_link(
    executable: "/usr/local/bin/duckport",
    args: ["--jsonl", "--database", "/var/db/app/analytics.duckdb"]
  )

{:ok, %{"rows" => [%{"answer" => 42}]}} =
  JsonlPort.request(pid, "query", %{sql: "SELECT 42 AS answer", params: []})

{:error, "missing_sql"} = JsonlPort.request(pid, "query", %{params: []})
```

Options of `start_link/1`:

- `:executable` - absolute path of the program (required).
- `:args` - its arguments.
- `:name` - registers the process.

`JsonlPort` can be started under a supervisor as `{JsonlPort, options}`.

## Behaviour

- Each process owns one program, started on the first request and again on
  the next request after it exits.
- Requests are serialised: each waits until the one before it has been
  answered. `request/4` takes a timeout in milliseconds (default 30 000).
- A reply is accepted only if it carries the request's `request_id` and
  protocol version 1. Requests and replies are limited to 16 MiB.
- A request that gets no valid reply in time returns
  `{:error, :uncertain_native_outcome}`, and the program is killed before
  the next request starts, on Windows with `taskkill`. A write the program
  was still doing may or may not have happened, so treat the outcome as
  unknown, not as failed.
- `reset/2` kills the program after any request in progress; the next
  request starts it again.

The errors are listed in the module documentation (`t:JsonlPort.error/0`).

## License

BSD 3-Clause License, see the LICENSE file.
