#!/usr/bin/env python3
"""Coverage thresholds and regression gate (README 8.1, OPS-22).

Input is the summary JSON of `cargo llvm-cov report --json --summary-only`.
Rules:
  - workspace line coverage >= WORKSPACE_MIN (80);
  - rustryx-auth, rustryx-rbac and every format parser crate >= STRICT_MIN (90);
  - regression: no drop against the baseline taken from main, for the workspace
    and for each strict crate. TOLERANCE absorbs rounding, not real losses.
A missing baseline (first run on a new repository or an expired cache) passes
with a notice, because there is nothing to compare with.

Exit codes: 0 ok, 1 threshold or regression, 3 the report is empty or unreadable
(zero measured lines is a broken run, never "100%").
Usage: coverage-gate.py [--baseline baseline.json] [--save-summary out.json]
                        [--summary FILE] current.json
"""

import argparse
import json
import re
import sys

WORKSPACE_MIN = 80.0
STRICT_MIN = 90.0
TOLERANCE = 0.1  # percentage points
# Parser crates are the format crates except format-core, which is the shared
# pipeline and is covered by the workspace rule only.
STRICT_CRATES = re.compile(r"^rustryx-(auth|rbac|format-(maven|npm|pypi|oci|helm|raw|pub))$")
CRATE_OF_FILE = re.compile(r"/crates/(rustryx-[a-z0-9-]+)/")


def per_crate(report: dict) -> dict:
    files = report["data"][0]["files"]
    totals: dict = {}
    for entry in files:
        match = CRATE_OF_FILE.search(entry["filename"].replace("\\", "/"))
        if not match:
            continue
        lines = entry["summary"]["lines"]
        count, covered = totals.get(match.group(1), (0, 0))
        totals[match.group(1)] = (count + lines["count"], covered + lines["covered"])
    return totals


def percent(count: int, covered: int) -> float:
    return 100.0 * covered / count


def measure(path: str) -> tuple:
    with open(path, encoding="utf-8") as fh:
        report = json.load(fh)
    crates = per_crate(report)
    total_count = sum(c for c, _ in crates.values())
    total_cov = sum(v for _, v in crates.values())
    if total_count == 0:
        raise ValueError("no instrumented lines in crates/")
    return percent(total_count, total_cov), {k: percent(c, v) for k, (c, v) in crates.items() if c}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("current")
    parser.add_argument("--baseline")
    parser.add_argument("--save-summary", help="write the measured percentages (the next baseline)")
    parser.add_argument("--summary", help="append a markdown summary to this file")
    args = parser.parse_args()

    try:
        workspace, crates = measure(args.current)
    except (OSError, ValueError, KeyError, IndexError) as exc:
        print(f"coverage-gate: unusable report {args.current}: {exc}", file=sys.stderr)
        return 3

    problems, lines = [], [f"workspace line coverage {workspace:.2f}% (minimum {WORKSPACE_MIN}%)"]
    if workspace < WORKSPACE_MIN:
        problems.append(f"workspace {workspace:.2f}% is below {WORKSPACE_MIN}%")
    for name in sorted(crates):
        strict = bool(STRICT_CRATES.match(name))
        lines.append(f"  {name:<28} {crates[name]:6.2f}%" + (f"  (minimum {STRICT_MIN}%)" if strict else ""))
        if strict and crates[name] < STRICT_MIN:
            problems.append(f"{name} {crates[name]:.2f}% is below {STRICT_MIN}%")

    if args.baseline:
        try:
            with open(args.baseline, encoding="utf-8") as fh:
                base = json.load(fh)
        except OSError:
            lines.append("no baseline from main available: regression check skipped")
        else:
            if workspace + TOLERANCE < base["workspace"]:
                problems.append(f"workspace dropped from {base['workspace']:.2f}% to {workspace:.2f}%")
            for name, old in base["crates"].items():
                if STRICT_CRATES.match(name) and name in crates and crates[name] + TOLERANCE < old:
                    problems.append(f"{name} dropped from {old:.2f}% to {crates[name]:.2f}%")
            lines.append(f"compared with main: workspace {base['workspace']:.2f}%")

    if args.save_summary:
        with open(args.save_summary, "w", encoding="utf-8") as fh:
            json.dump({"workspace": workspace, "crates": crates}, fh, indent=2)
    print("\n".join(lines))
    for problem in problems:
        print(f"coverage-gate: {problem}", file=sys.stderr)
    if args.summary:
        with open(args.summary, "a", encoding="utf-8") as fh:
            fh.write("### Coverage\n\n```\n" + "\n".join(lines + [f"FAIL {p}" for p in problems]) + "\n```\n")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
