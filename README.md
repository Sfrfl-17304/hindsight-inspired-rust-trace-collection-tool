# Cooperative, retroactive trace collection

> *What if every node kept its own traces, and nodes only talked to each
> other when something went wrong?*

Course project for CS-E4660 Advanced Topics in Software Systems, inspired by
[Hindsight (NSDI'23)](https://www.usenix.org/conference/nsdi23/presentation/zhang-lei).

Today, distributed tracing ships every span to a central collector and then
samples it down, often before knowing which traces matter, so rare failures
get lost. In Rewind, every node keeps its spans in a fixed-size in-memory
buffer and sends nothing by default. When a node sees a problem, it triggers
the trace: it reports its own spans and asks the neighbouring nodes that
handled the same request to do the same, following the request one hop at a
time. The project then tests whether this cooperation still works when the
system is being broken on purpose (chaos engineering).

## What is a node?

**A node is one service instance plus its own Rust tracing agent, deployed and
failing as one unit** (two containers sharing a network namespace, like a
Kubernetes pod). It owns a fixed trace-memory budget and its own network link.
Hosts (laptop, VMs) only decide *where* nodes run. Full rationale, including
the alternatives and the difference from Hindsight: [docs/node-model.md](docs/node-model.md).

## Architecture

```
loadgen ──▶ frontend ──▶ orders ──▶ payments        (Go services, HTTP)
              │ agent      │ agent      │ agent      (Rust, one per node, same netns)
              └──── agent ↔ agent: "collect trace X" (breadcrumbs) ────┘
                              │ spans of triggered traces only
                              ▼
                          collector                  (Rust, assembles traces)
```

1. **Spans stay local.** Each Go service hands finished spans to its agent on
   `127.0.0.1:9000`. The agent stores them in a ring buffer with a byte budget
   (default 16 MB); when full, it overwrites the oldest trace.
2. **Breadcrumbs.** Requests carry the caller's agent address
   (`X-Rewind-Agent`) and responses carry the callee's, so every span knows its
   neighbouring nodes.
3. **Trigger.** An error or a span slower than a threshold triggers the trace.
   The agent *locks* it (takes it out of the ring so it cannot be overwritten),
   reports its spans to the collector and forwards the trigger to its
   breadcrumbs. Each node collects a trace at most once.
4. **Late spans.** Callers usually finish *after* the callee failed, so their
   spans arrive at an already-triggered agent. These are forwarded at once,
   and their breadcrumbs carry the trigger further upstream.

| Path | Contents |
|---|---|
| [`tracer/`](tracer) | Rust crate: `rewind-agent`, `rewind-collector`, ring buffer, tests |
| [`services/`](services) | Go: configurable microservice + load generator (ground truth) |
| [`docker-compose.yml`](docker-compose.yml) | 3 nodes + collector, one-line switch between modes |
| [`experiments/`](experiments) | Fault injection (`fault.py`), experiment runners (`chaos.py`, `compare_modes.py`), `summarize.py` |
| [`results/`](results) | Raw ground truth of every run and the summary CSVs |
| [`docs/`](docs) | [Node model](docs/node-model.md) · [Architecture](docs/architecture.md) · [Protocol](docs/protocol.md) · [Experiment plan](docs/experiments.md) |

## Running it

Requires Docker. Rust is only needed to run the tests locally.

```sh
docker compose up -d --build
docker compose run --rm loadgen -rps 50 -duration 20s   # writes results/ground_truth.csv
```

Open **http://localhost:9100**: the collector's viewer lists the collected
traces, draws each one as a timeline, and shows every node's agent (spans
seen, buffered, evicted, triggered). Click a node to see the healthy traces
it still holds locally. The same data as JSON:

```sh
curl localhost:9100/v1/traces          # what the collector received
curl localhost:9100/v1/traces/<id>     # one assembled trace
curl localhost:9003/v1/stats           # payments agent: buffer + network counters
```

Inject faults into a node with `python experiments/fault.py` (delay, packet
loss, crash; see `python experiments/fault.py` for usage). A step-by-step demo
is in [DEMO.md](DEMO.md).

Baselines run on the same code path:
`REWIND_MODE=all docker compose up -d` (send everything) or
`REWIND_MODE=sample:0.01 docker compose up -d` (1% head sampling).

Tests: `cd tracer && cargo test` (ring buffer unit tests, plus an end-to-end
test with 3 agents and a collector on localhost).

## Status

**Implemented**
- Node model defined ([docs/node-model.md](docs/node-model.md))
- Rust agent: ring buffer with byte budget and eviction, trigger on error or
  latency, breadcrumb fan-out, late-span forwarding, at-most-once collection per node
- Rust collector: assembles traces, removes duplicate spans, counts bytes
  received, and serves a trace viewer
- Go services with breadcrumb propagation, and a load generator that records the ground truth
- Docker Compose deployment, one pod-like node per service, with per-node CPU and memory limits
- Baseline modes `all` and `sample:p` in the same agent
- Per-node fault injection (delay, packet loss, crash) and an experiment runner

## Results under fault injection

Each scenario: 3 runs of 1000 requests (50 requests/s for 20 s); payments
fails 1% of requests. An anomalous request failed or took over 500 ms; it is
caught if the collector holds its trace complete (one span per service that
handled it). Totals over the 3 runs:

| Scenario | Fault | Rewind: caught | Full export: caught | 1% sampling: caught | Rewind KB / run | Full export KB / run |
|---|---|---:|---:|---:|---:|---:|
| none | none | 25 / 25 | 27 / 27 | 0 / 31 | 7.1 | 748 |
| delay | orders +150 ms ± 60 ms | 145 / 150 | 147 / 147 | 2 / 139 | 43.7 | 746 |
| loss | payments 20% packet loss | 63 / 63 | 70 / 70 | 1 / 58 | 17.9 | 748 |
| crash | payments node killed for 5 s | 1053 / 1053 | 1055 / 1055 | 18 / 1054 | 200 | 645 |
| small-buffer | delay, 16 KB buffers | 131 / 133 | 148 / 148 | 2 / 163 | 39.4 | 746 |
| tiny-buffer | delay, 4 KB buffers | 39 / 156 (113 partial) | n/a | n/a | 25.4 | n/a |

Observations:
- Under delay, packet loss and a node crash, Rewind collected the same
  anomalous traces as full export, at 1–31% of its data volume (the share
  grows with the number of anomalies; during the crash a third of all
  requests failed). Head sampling missed almost all of them.
- Rewind's misses under delay (5 of 150) were whole traces with no spans at
  all, never partial ones. Every run had a few requests just over 500 ms at
  the load generator, most likely with a frontend span just under 500 ms,
  so no node saw them as slow: a mismatch between where the experiment and
  the agents measure latency, not a lost trace.
- The buffer must outlast the trigger. 16 KB per node (about one second of
  spans at this load) was enough; with 4 KB (about a quarter of a second)
  most anomalous traces arrived partial, because upstream spans were
  overwritten before the trigger reached them. Full export and sampling do
  not buffer, so this scenario was run for Rewind only.

Reproduce with `python experiments/chaos.py --reps 3` (about 25 minutes,
Docker running), then `python experiments/summarize.py`. Raw ground truth of
every run is in `results/chaos/`, per-run numbers in
`results/chaos_summary.csv`.

## Checkpoint 2 measurement (fault-free)

No faults, 1000 requests at 50 requests/s, payments fails 1% of requests,
one run per configuration:

| Mode | Failed requests | Failed traces collected complete | Traces at collector | Bytes to collector |
|---|---:|---:|---:|---:|
| Rewind (`retro`) | 17 | 17 | 17 | 14.6 KB |
| Full export (`all`) | 11 | 11 | 1000 | 748 KB |
| Head sampling 1% | 9 | 0 | 10 | 8.5 KB |

Rewind retained every failed trace at about 2% of the data volume of full
export; head sampling had a comparable volume but retained none. Failure counts
differ between runs, and these are fault-free conditions, so the results are
indicative only.

Reproduce with `python experiments/compare_modes.py` (Docker running). It
writes `results/summary.csv` plus the ground truth of each run
(`results/ground_truth_<mode>.csv`). The numbers above are from the run of
18.11.2026, committed in `results/`.

## Limitations and future work
- Only one node at a time is faulted, on a single host; no CPU-overload
  scenario and no rerun across several VMs yet
- Buffer size was tested at three points (16 MB, 16 KB, 4 KB), not swept
- Agent CPU and memory overhead is not measured yet
- Bytes are counted at the collector; trigger messages between agents are not counted
- JSON over HTTP between service and agent instead of OTLP or shared memory
- "Asking twice" is handled per node (a node collects a trace at most once) and
  by the collector removing duplicate spans, with no global coordination
- JSON over HTTP between service and agent instead of OTLP or shared memory
- "Asking twice" is handled per node (a node collects a trace at most once) and
  by the collector removing duplicate spans, with no global coordination
