# Rewind: cooperative, retroactive trace collection

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
| [`docs/`](docs) | [Node model](docs/node-model.md) · [Architecture](docs/architecture.md) · [Protocol](docs/protocol.md) · [Experiment plan](docs/experiments.md) |

## Running it

Requires Docker. Rust is only needed to run the tests locally.

```sh
docker compose up -d --build
docker compose run --rm loadgen -rps 50 -duration 20s   # writes results/ground_truth.csv
curl localhost:9100/v1/traces          # what the collector received
curl localhost:9100/v1/traces/<id>     # one assembled trace
curl localhost:9003/v1/stats           # payments agent: buffer + network counters
```

Baselines run on the same code path:
`REWIND_MODE=all docker compose up -d` (send everything) or
`REWIND_MODE=sample:0.01 docker compose up -d` (1% head sampling).

Tests: `cd tracer && cargo test` (ring buffer unit tests, plus an end-to-end
test with 3 agents and a collector on localhost).

## Status (Checkpoint 2)

**Done**
- Node model defined ([docs/node-model.md](docs/node-model.md))
- Rust agent: ring buffer with byte budget and eviction, trigger on error or
  latency, breadcrumb fan-out, late-span forwarding, at-most-once collection per node
- Rust collector: assembles traces, removes duplicate spans, counts bytes received
- Go services with breadcrumb propagation, and a load generator that records the ground truth
- Docker Compose deployment, one pod-like node per service, with per-node CPU and memory limits
- Baseline modes `all` and `sample:p` in the same agent

**First measurement** (no faults, 1000 requests at 50 rps, payments fails 1% of the time):

| Mode | Failed requests | Failed requests collected complete | Traces at collector | Bytes to collector |
|---|---:|---:|---:|---:|
| Rewind (`retro`) | 11 | **11** | 11 | **9.4 KB** |
| Send everything (`all`) | 9 | 9 | 1000 | 748 KB |
| Head sampling 1% | 18 | **0** | 6 | 5.1 KB |

Rewind catches every failure, as sending everything does, at about 1/80 of
the traffic. Sampling costs about as little as Rewind but misses every
failure. *This is the happy path; the open question is what happens under faults.*

**Next (final week)**
- Chaos: `tc netem` delay and loss per node, `docker kill` of a node, CPU overload
- Memory pressure: shrink `REWIND_BUFFER_BYTES` until traces are overwritten before collection
- Metrics script: missed-failure rate per fault × mode, CPU, memory and network per node
- Optional: rerun across 2–3 VMs

**Known simplifications**
- JSON over HTTP between service and agent instead of OTLP or shared memory
- "Asking twice" is handled per node (a node collects a trace at most once) and
  by the collector removing duplicate spans, with no global coordination
