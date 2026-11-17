# Experiment plan

## Question

Does cooperative, retroactive trace collection still deliver the traces of
failing requests when the system itself is failing — and at what cost?

## Method: ground truth first

The load generator chooses every request's trace id itself and writes
`results/ground_truth.csv`: trace id, start time, HTTP status, latency.
Because we also inject the faults ourselves, we know for every request
whether it failed and *why* — so "did tracing catch it?" is a lookup, not a
guess.

A failed trace counts as **caught** only if the collector holds it
*complete* (one span per node on its path). Everything else is a miss.

## Modes (same agent binary, `REWIND_MODE`)

| Mode | Meaning |
|---|---|
| `retro` | Rewind: keep locally, collect cooperatively on trigger |
| `all` | baseline: every span goes to the collector |
| `sample:0.01` | baseline: 1% head sampling (deterministic per trace id, like real tracers) |

## Metrics

| Metric | Source |
|---|---|
| Missed-failure rate | ground truth CSV vs `GET /v1/traces` |
| Trace completeness | spans per collected trace vs path length |
| Network cost | collector `bytes_received`; agent `report_bytes_sent` |
| Agent overhead | agent `/v1/stats` (buffer use, evictions) + `docker stats` (CPU, memory) |
| Collection race | triggered-but-evicted traces (buffer stats) |

## Fault matrix (final week)

Each cell: ~2 min of load at 50 rps, each mode, 3 repetitions.

| Fault | Injection | What it stresses |
|---|---|---|
| none (happy path) | — | baseline numbers (done, see README) |
| network delay | `tc netem delay` on one node | triggers race the dashcam loop |
| packet loss | `tc netem loss` | lost triggers/reports |
| node crash | `docker kill`, restart after N s | spans die with the node |
| overload | loadgen at high rps | buffer churn, eviction before collection |
| small memory | `REWIND_BUFFER_BYTES` sweep | how much memory a node really needs |

Faults are injected *per node* (inside its network namespace), so they hit
the service and its agent together — the tracer is never exempt from the
failure it is observing.

## Status

* Happy-path comparison done — table in the [README](../README.md#status-checkpoint-2).
* Fault injection scripts: next.
