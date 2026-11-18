"""Compare Rewind with the two baselines under fault-free conditions.

For each mode (retro, all, sample:0.01) this script:
  1. restarts every agent in that mode (docker compose),
  2. clears the collector,
  3. runs the load generator, which writes the ground truth CSV,
  4. compares the ground truth with what the collector received.

Output: results/summary.csv (one row per mode) and a table on stdout.
The ground truth of each run is kept in results/ground_truth_<mode>.csv.

Usage (from the repository root, with Docker running):
    python experiments/compare_modes.py [--rps 50] [--duration 20s]
"""
import argparse
import csv
import json
import os
import subprocess
import time
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RESULTS = ROOT / "results"
COLLECTOR = "http://localhost:9100"
MODES = ["retro", "all", "sample:0.01"]
PATH_LENGTH = 3  # frontend -> orders -> payments: a complete trace has 3 spans


def http(method, url):
    req = urllib.request.Request(url, method=method)
    with urllib.request.urlopen(req, timeout=10) as resp:
        body = resp.read()
        return json.loads(body) if body else None


def compose(*args, mode):
    env = dict(os.environ, REWIND_MODE=mode)
    subprocess.run(["docker", "compose", *args], cwd=ROOT, env=env, check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def wait_for_collector():
    for _ in range(50):
        try:
            urllib.request.urlopen(f"{COLLECTOR}/healthz", timeout=1)
            return
        except OSError:
            time.sleep(0.2)
    raise RuntimeError("collector did not come up")


def run_mode(mode, rps, duration):
    compose("up", "-d", mode=mode)
    wait_for_collector()
    time.sleep(2)  # let the agents start
    http("DELETE", f"{COLLECTOR}/v1/traces")

    name = mode.replace(":", "_")
    truth_file = RESULTS / f"ground_truth_{name}.csv"
    compose("run", "--rm", "loadgen", "-rps", str(rps), "-duration", duration,
            "-out", f"/results/ground_truth_{name}.csv", mode=mode)
    time.sleep(2)  # let the last reports arrive

    with open(truth_file, newline="") as f:
        rows = list(csv.DictReader(f))
    failed = {r["trace_id"] for r in rows if r["status"] != "200"}

    traces = {t["trace_id"]: t["span_count"] for t in http("GET", f"{COLLECTOR}/v1/traces")}
    stats = http("GET", f"{COLLECTOR}/v1/stats")
    complete = sum(1 for t in failed if traces.get(t) == PATH_LENGTH)

    return {
        "mode": mode,
        "requests": len(rows),
        "failed_requests": len(failed),
        "failed_traces_collected_complete": complete,
        "traces_at_collector": len(traces),
        "bytes_to_collector": stats["bytes_received"],
        "reports_to_collector": stats["reports_received"],
        "rps": rps,
        "duration": duration,
        "run_at_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--rps", type=int, default=50)
    ap.add_argument("--duration", default="20s")
    args = ap.parse_args()

    RESULTS.mkdir(exist_ok=True)
    rows = []
    try:
        for mode in MODES:
            print(f"running mode {mode} ...", flush=True)
            rows.append(run_mode(mode, args.rps, args.duration))
    finally:
        compose("up", "-d", mode="retro")  # leave the system in its default mode

    with open(RESULTS / "summary.csv", "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(rows[0]))
        w.writeheader()
        w.writerows(rows)

    print(f"\n{'mode':<12} {'requests':>8} {'failed':>7} {'caught':>7} {'traces':>7} {'bytes':>9}")
    for r in rows:
        print(f"{r['mode']:<12} {r['requests']:>8} {r['failed_requests']:>7} "
              f"{r['failed_traces_collected_complete']:>7} {r['traces_at_collector']:>7} "
              f"{r['bytes_to_collector']:>9}")
    print(f"\nwritten to {RESULTS / 'summary.csv'}")


if __name__ == "__main__":
    main()
