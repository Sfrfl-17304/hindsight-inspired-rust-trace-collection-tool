"""Summarise results/chaos_summary.csv: totals over repetitions, per scenario and mode.

    python experiments/summarize.py            # plain table
    python experiments/summarize.py --markdown # table for the README
"""
import csv
import sys
from collections import defaultdict
from pathlib import Path

SUMMARY = Path(__file__).resolve().parent.parent / "results" / "chaos_summary.csv"
FIELDS = ["anomalous", "caught", "partial", "missed", "bytes_to_collector"]


def main():
    with open(SUMMARY, newline="") as f:
        rows = list(csv.DictReader(f))
    totals = defaultdict(lambda: defaultdict(int))
    order = []
    for r in rows:
        key = (r["scenario"], r["mode"])
        if key not in totals:
            order.append(key)
        totals[key]["runs"] += 1
        for k in FIELDS:
            totals[key][k] += int(r[k])

    md = "--markdown" in sys.argv
    head = ["scenario", "mode", "runs", "anomalous", "caught", "partial", "missed", "caught %", "KB / run"]
    if md:
        print("| " + " | ".join(head) + " |")
        print("|" + "---|" * 2 + "---:|" * (len(head) - 2))
    else:
        print(f"{'scenario':<13} {'mode':<12} {'runs':>4} {'anomalous':>9} {'caught':>7} "
              f"{'partial':>7} {'missed':>7} {'caught %':>8} {'KB/run':>8}")
    for scenario, mode in order:
        t = totals[(scenario, mode)]
        pct = 100 * t["caught"] / t["anomalous"] if t["anomalous"] else 100.0
        kb = t["bytes_to_collector"] / t["runs"] / 1000
        cells = [scenario, mode, t["runs"], t["anomalous"], t["caught"], t["partial"], t["missed"],
                 f"{pct:.1f}", f"{kb:.1f}"]
        if md:
            print("| " + " | ".join(str(c) for c in cells) + " |")
        else:
            print(f"{scenario:<13} {mode:<12} {t['runs']:>4} {t['anomalous']:>9} {t['caught']:>7} "
                  f"{t['partial']:>7} {t['missed']:>7} {pct:>8.1f} {kb:>8.1f}")


if __name__ == "__main__":
    main()
