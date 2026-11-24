"""Fault-injection experiment: Rewind against the two baselines, per fault.

For every scenario x mode (x repetition) this script:
  1. restarts the agents in that mode (and buffer size),
  2. clears faults and the collector, then injects the scenario's fault,
  3. runs the load generator, which writes the ground truth,
  4. compares the ground truth with what the collector received.

An *anomalous* request is one that failed (status != 200) or was slower than
the agents' latency threshold. It is *caught* if the collector holds its trace
with at least as many spans as services that handled it (the `hops` column
reported back to the load generator), *partial* if some spans arrived, and
*missed* otherwise.

Usage (repository root, Docker running):
    python experiments/chaos.py                       # every scenario, 1 repetition
    python experiments/chaos.py --reps 3 --duration 30s
    python experiments/chaos.py --only none,crash
Output: results/chaos_summary.csv, ground truth per run in results/chaos/.
"""
import argparse
import csv
import json
import os
import threading
import time
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

import fault

ROOT = Path(__file__).resolve().parent.parent
RESULTS = ROOT / "results"
COLLECTOR = "http://localhost:9100"
MODES = ["retro", "all", "sample:0.01"]
LATENCY_THRESHOLD_MS = 500
DEFAULT_BUFFER = 16 * 1024 * 1024


def _delay_orders():
    # Calibrated so that about 5% of requests exceed the 500 ms threshold:
    # a latency tail, not a uniformly slow system.
    fault.delay("orders", "150ms", "60ms")


# name -> (description, buffer bytes, fault applied before the load, fault during the load)
SCENARIOS = {
    "none": ("no fault", DEFAULT_BUFFER, None, None),
    "delay": ("orders +150 ms (+-60 ms)", DEFAULT_BUFFER, _delay_orders, None),
    "loss": ("payments 20% packet loss", DEFAULT_BUFFER, lambda: fault.loss("payments", "20%"), None),
    "crash": ("payments node killed for 5 s", DEFAULT_BUFFER, None,
              lambda duration: (time.sleep(duration / 3), fault.crash("payments", 5))),
    "small-buffer": ("orders +150 ms (+-60 ms), 16 KB buffers", 16 * 1024, _delay_orders, None),
    "tiny-buffer": ("orders +150 ms (+-60 ms), 4 KB buffers", 4 * 1024, _delay_orders, None),
}


def http(method, url):
    req = urllib.request.Request(url, method=method)
    with urllib.request.urlopen(req, timeout=10) as resp:
        body = resp.read()
        return json.loads(body) if body else None


def wait_for(url):
    for _ in range(100):
        try:
            urllib.request.urlopen(url, timeout=1)
            return
        except OSError:
            time.sleep(0.2)
    raise RuntimeError(f"{url} did not come up")


def seconds(duration):
    return float(duration.rstrip("s"))


def run_once(scenario, mode, rep, rps, duration):
    _, buffer_bytes, before, during = SCENARIOS[scenario]
    # fault.py calls docker compose too (crash restarts agents): give it the same settings.
    os.environ.update(REWIND_MODE=mode, REWIND_BUFFER_BYTES=str(buffer_bytes))
    fault.compose("up", "-d", capture=True)
    wait_for(f"{COLLECTOR}/healthz")
    fault.clear()
    time.sleep(2)  # let the agents start
    http("DELETE", f"{COLLECTOR}/v1/traces")
    if before:
        before()

    name = f"{scenario}_{mode.replace(':', '_')}_{rep}"
    truth = RESULTS / "chaos" / f"gt_{name}.csv"
    injector = None
    if during:
        injector = threading.Thread(target=during, args=(seconds(duration),))
        injector.start()
    fault.compose("run", "--rm", "loadgen", "-rps", str(rps), "-duration", duration,
                  "-out", f"/results/chaos/gt_{name}.csv", capture=True)
    if injector:
        injector.join()
    fault.clear()
    time.sleep(3)  # let the last triggers and reports arrive

    with open(truth, newline="") as f:
        rows = list(csv.DictReader(f))
    anomalous = {r["trace_id"]: max(int(r["hops"]), 1) for r in rows
                 if r["status"] != "200" or float(r["latency_ms"]) > LATENCY_THRESHOLD_MS}
    traces = {t["trace_id"]: t["span_count"] for t in http("GET", f"{COLLECTOR}/v1/traces")}
    caught = sum(1 for t, hops in anomalous.items() if traces.get(t, 0) >= hops)
    partial = sum(1 for t, hops in anomalous.items() if 0 < traces.get(t, 0) < hops)
    stats = http("GET", f"{COLLECTOR}/v1/stats")

    return {
        "scenario": scenario,
        "mode": mode,
        "rep": rep,
        "requests": len(rows),
        "failed": sum(1 for r in rows if r["status"] != "200"),
        "anomalous": len(anomalous),
        "caught": caught,
        "partial": partial,
        "missed": len(anomalous) - caught - partial,
        "traces_at_collector": len(traces),
        "bytes_to_collector": stats["bytes_received"],
        "buffer_bytes": buffer_bytes,
        "rps": rps,
        "duration": duration,
        "run_at_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--rps", type=int, default=50)
    ap.add_argument("--duration", default="20s")
    ap.add_argument("--reps", type=int, default=1)
    ap.add_argument("--only", help="comma-separated scenarios (default: all)")
    ap.add_argument("--modes", help=f"comma-separated modes (default: {','.join(MODES)})")
    args = ap.parse_args()

    scenarios = args.only.split(",") if args.only else list(SCENARIOS)
    modes = args.modes.split(",") if args.modes else MODES
    (RESULTS / "chaos").mkdir(parents=True, exist_ok=True)
    out = RESULTS / "chaos_summary.csv"
    # Keep earlier results, except the runs this invocation repeats.
    redo = {(s, m, str(r)) for s in scenarios for m in modes for r in range(1, args.reps + 1)}
    kept = []
    if out.exists():
        with open(out, newline="") as f:
            kept = [r for r in csv.DictReader(f) if (r["scenario"], r["mode"], r["rep"]) not in redo]
    rows = []
    try:
        for scenario in scenarios:
            for rep in range(1, args.reps + 1):
                for mode in modes:
                    print(f"{scenario:<13} {mode:<12} rep {rep} ...", end=" ", flush=True)
                    r = run_once(scenario, mode, rep, args.rps, args.duration)
                    rows.append(r)
                    print(f"anomalous {r['anomalous']:>4}  caught {r['caught']:>4}  "
                          f"partial {r['partial']:>3}  missed {r['missed']:>4}  "
                          f"bytes {r['bytes_to_collector']:>9}", flush=True)
                    with open(out, "w", newline="") as f:  # rewrite after every run
                        w = csv.DictWriter(f, fieldnames=list(rows[0]))
                        w.writeheader()
                        w.writerows(kept + rows)
    finally:
        fault.clear()
        os.environ.update(REWIND_MODE="retro", REWIND_BUFFER_BYTES=str(DEFAULT_BUFFER))
        fault.compose("up", "-d", capture=True)  # leave the system in its default state
    print(f"\nwritten to {out}")


if __name__ == "__main__":
    main()
