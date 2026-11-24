"""Inject faults into one node of the running system.

A node is a service container plus its agent, sharing one network namespace,
so every fault below hits the service and its tracing agent together.

    python experiments/fault.py delay orders 300ms [100ms]   # added latency (+ jitter)
    python experiments/fault.py loss payments 20%            # packet loss
    python experiments/fault.py crash payments 5             # kill node, restart after 5 s
    python experiments/fault.py kill payments                # kill node (no restart)
    python experiments/fault.py restart payments             # bring a killed node back
    python experiments/fault.py clear [node]                 # remove network faults (default: all)
    python experiments/fault.py status                       # show active network faults

Network faults use `tc netem` on the node's eth0, i.e. on everything the node
sends: its service's responses and calls, and its agent's triggers and reports.
Traffic between a service and its own agent stays on localhost and is not
affected, exactly as on a real host.
"""
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
NODES = ["frontend", "orders", "payments"]


def compose(*args, check=True, capture=False):
    return subprocess.run(["docker", "compose", *args], cwd=ROOT, check=check,
                          capture_output=capture, text=True)


def tc(node, *args, check=True):
    return compose("exec", "-T", node, "tc", "qdisc", *args, check=check, capture=True)


def _check_node(node):
    if node not in NODES:
        sys.exit(f"unknown node {node!r}, expected one of {NODES}")


def delay(node, ms, jitter=None):
    _check_node(node)
    args = ["delay", ms] + ([jitter, "distribution", "normal"] if jitter else [])
    tc(node, "replace", "dev", "eth0", "root", "netem", *args)


def loss(node, pct):
    _check_node(node)
    tc(node, "replace", "dev", "eth0", "root", "netem", "loss", pct)


def clear(node=None):
    for n in [node] if node else NODES:
        tc(n, "del", "dev", "eth0", "root", check=False)  # fails harmlessly if none


def kill(node):
    _check_node(node)
    compose("kill", node, f"{node}-agent", capture=True)


def restart(node):
    _check_node(node)
    # The agent joins the service's network namespace, so start the service first.
    compose("up", "-d", node, capture=True)
    compose("up", "-d", "--force-recreate", f"{node}-agent", capture=True)


def crash(node, seconds):
    kill(node)
    time.sleep(seconds)
    restart(node)


def status():
    for n in NODES:
        out = tc(n, "show", "dev", "eth0", check=False).stdout.strip()
        netem = [line for line in out.splitlines() if "netem" in line]
        print(f"{n:<9} {netem[0] if netem else 'no network fault'}")


def main(argv):
    if not argv:
        sys.exit(__doc__)
    cmd, args = argv[0], argv[1:]
    try:
        if cmd == "delay":
            delay(*args[:3])
        elif cmd == "loss":
            loss(args[0], args[1])
        elif cmd == "crash":
            crash(args[0], float(args[1]) if len(args) > 1 else 5)
        elif cmd == "kill":
            kill(args[0])
        elif cmd == "restart":
            restart(args[0])
        elif cmd == "clear":
            clear(args[0] if args else None)
        elif cmd == "status":
            status()
        else:
            sys.exit(__doc__)
    except (IndexError, TypeError):
        sys.exit(__doc__)
    except subprocess.CalledProcessError as e:
        sys.exit(f"failed: {' '.join(e.cmd)}\n{e.stderr or ''}")
    if cmd not in ("status",):
        print(f"ok: {' '.join(argv)}")


if __name__ == "__main__":
    main(sys.argv[1:])
