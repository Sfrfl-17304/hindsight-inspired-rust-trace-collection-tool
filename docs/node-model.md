# What is a node?

> **A node is one service instance together with its own Rust tracing agent,
> deployed and failing as one unit.** It has its own trace memory (a
> fixed-size ring buffer), its own network link, and it crashes as a whole.
> It is a *logical* node, not a physical machine: concretely, two containers
> sharing one network namespace, the same shape as a Kubernetes pod.

```
 ┌──────── node: frontend ────────┐      ┌──────── node: orders ──────────┐      ┌──────── node: payments ────────┐
 │ frontend (Go)  :8080           │ HTTP │ orders (Go)  :8080             │ HTTP │ payments (Go)  :8080           │
 │   │ spans → 127.0.0.1:9000     │─────▶│   │                            │─────▶│   │  error → trigger           │
 │ agent (Rust)   :9000           │      │ agent (Rust)  :9000            │      │ agent (Rust)   :9000           │
 │   ring buffer (16 MB)          │◀────▶│   ring buffer (16 MB)          │◀────▶│   ring buffer (16 MB)          │
 └────────────────────────────────┘      └────────────────────────────────┘      └────────────────────────────────┘
                     agent ↔ agent: "collect trace X" (follows breadcrumbs)
                     agent → collector: the node's spans of trace X
```

## Three levels, kept separate

| Level | What it is here | Role in the protocol |
|---|---|---|
| **Service instance** | One Go process (`services/cmd/service`) | Produces spans, leaves breadcrumbs |
| **Node** | Service + agent in one network namespace | Unit of memory, of failure, of message passing |
| **Host** | The laptop (development) or 2–3 VMs (final runs) | Only *where* nodes run; invisible to the protocol |

## Why not the alternatives

| Option | Verdict | Reason |
|---|---|---|
| Physical machine | ✗ | Not enough machines; and several services on one machine would cooperate without any network in between. |
| VM | ✗ as definition, ✓ as host | A VM just hosts nodes. Spreading nodes over VMs changes nothing in the protocol. |
| Microservice alone | ✗ | Incomplete: the agent and its buffer belong to the node. If the node crashes, its part of the trace is lost, which is one of the faults to test. |
| **Service + agent** | ✓ | The neighbours in the protocol are the services in the call graph, so the node follows the service. Faults can target one node (netem on its interface, `docker kill`, CPU limit) and, because the agent shares the namespace, **the faults also hit the tracer**. |

## Difference from Hindsight

Hindsight (NSDI'23) runs one agent per *machine*, shared by all processes on it.
Rewind runs one agent per *service instance*, on purpose: every hop of a request
is then a hop between nodes over the network, which is exactly what the chaos
experiments slow down, drop and break.

## Known limitations

* Containers on one host share CPU and kernel, so failures are not fully
  independent. Mitigation: per-node `cpus` / `mem_limit` and per-node netem;
  final runs optionally repeated across 2–3 VMs.
* Upside of one host: all nodes share one clock, so "collected before it was
  overwritten" can be measured exactly.
