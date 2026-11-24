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

A request is **anomalous** if it failed (status ≠ 200) or took longer than
the agents' latency threshold (500 ms). Its trace counts as **caught** only
if the collector holds it *complete*: at least one span per service that
handled the request. Every service reports back how many services handled the
request (`X-Rewind-Hops` response header), and the load generator records it
in the `hops` column. This matters under faults: when payments is down, a
correct trace has two spans, not three. A trace with fewer spans is
**partial**, and one the collector never received is **missed**.

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

## Fault scenarios

Implemented in [`experiments/chaos.py`](../experiments/chaos.py); faults are
injected with [`experiments/fault.py`](../experiments/fault.py), which can
also be used by hand. Each run: 20 s of load at 50 requests/s, in each mode.

| Scenario | Injection | What it stresses |
|---|---|---|
| `none` | none | reference numbers |
| `delay` | `tc netem delay 150ms 60ms` on orders (about 5% of requests exceed 500 ms) | latency anomalies, triggered upstream (frontend) and propagated down |
| `loss` | `tc netem loss 20%` on payments | lost or late triggers, reports and responses |
| `crash` | `docker kill` of the payments node (service and agent), restart after 5 s | spans and triggers lost with the node |
| `small-buffer` | delay on orders, with 16 KB buffers instead of 16 MB | eviction before the trigger arrives (the collection race) |
| `tiny-buffer` | delay on orders, with 4 KB buffers (Rewind only: the baselines do not buffer) | the same race, past the point where it is lost |

Results: [README](../README.md#results-under-fault-injection).

Faults are injected *per node* (inside its network namespace), so they hit
the service and its agent together: the tracer is never exempt from the
failure it is observing. Traffic between a service and its own agent stays
on localhost and is not affected, as on a real host.

## Not covered

* CPU overload of a node, and a sweep over buffer sizes (only one small size).
* Repetitions: `--reps N` is supported; reported numbers state how many runs
  they come from.
* Deployment across several VMs.
