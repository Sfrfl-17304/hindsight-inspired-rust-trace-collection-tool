# Demo runbook

About 6 minutes. Commands are for PowerShell from the repository root; on
Linux/macOS use `curl` instead of `curl.exe`.

## Before the demo (5 min earlier)

```powershell
docker compose up -d --build        # 3 nodes (service + agent) + collector
python experiments/fault.py clear   # no leftover network faults
curl.exe -X DELETE localhost:9100/v1/traces
```

Open **http://localhost:9100** (the collector's viewer) next to the terminal.
It refreshes every second.

## 1. The system (30 s)

```powershell
docker compose ps
```

Seven containers: three nodes, each a Go service plus its Rust agent sharing
one network namespace, and the collector. The node cards at the top of the
viewer show each agent's buffer.

## 2. Normal operation: only failures leave the nodes (90 s)

```powershell
docker compose run --rm loadgen -rps 50 -duration 20s
```

While it runs, point at the viewer:

* each node card's **spans seen** climbs to about 1000, but **traces in
  buffer** stays local; nothing is sent for healthy requests;
* payments fails about 1% of requests; each failure appears in the trace list
  within a second;
* click a trace: three spans, frontend → orders → payments, error originating
  in payments; the bottom line shows the breadcrumbs the agents followed.

The load generator prints `sent 1000 requests, N failed`: N should equal the
number of traces in the viewer.

**Where are the healthy traces?** Click a node card: the right panel shows
the most recent spans held in that node's buffer, mostly `ok`. They never
left the node. Peeking is read-only and does not collect anything; when the
buffer is full they are overwritten.

## 3. Fault: network delay on one node (60 s)

```powershell
python experiments/fault.py delay orders 150ms 60ms
docker compose run --rm loadgen -rps 50 -duration 10s
python experiments/fault.py clear orders
```

Requests through orders are now slower, and about 5% exceed the 500 ms
latency threshold.
These traces show origin **slow**: they were triggered by the *frontend*
agent (its span was slow), and the trigger walked down to orders and
payments, whose spans were still in their buffers.

## 4. Fault: a node crashes (60 s)

Start the load in one terminal, crash payments from a second one:

```powershell
docker compose run --rm loadgen -rps 50 -duration 20s
python experiments/fault.py crash payments 5      # second terminal
```

The payments card turns red (agent unreachable) for about 5 s. Every request
in that window fails at orders; those traces arrive with two spans
(frontend, orders), which is complete, since payments never handled them.

## 5. Results (60 s)

```powershell
python experiments/summarize.py
```

Totals over the runs in `results/chaos_summary.csv`, produced by
`python experiments/chaos.py --reps 3` (about 25 minutes, do not run it
live). Explain one row per fault: Rewind versus full export versus 1% head
sampling.

## If something goes wrong

| Symptom | Fix |
|---|---|
| viewer empty after load | wait 2 s; check `docker compose logs --tail 20 payments-agent` |
| a node card stays red | `python experiments/fault.py restart payments` |
| requests suddenly slow | `python experiments/fault.py status`, then `clear` |
| anything else | `docker compose down; docker compose up -d` |
