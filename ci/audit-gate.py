#!/usr/bin/env python3
"""Severity gate over the JSON report of `cargo audit --json`.

Why a script and not `cargo audit` alone: README 8.6 asks to fail on high and
above. The cargo-audit config key `severity_threshold` removes lower findings
from the report, so the report would hide them. Here the whole report is
printed and only the threshold decides the exit code.

Exit codes (never merged, so a broken scanner cannot look like a finding):
  0  clean, or only findings below the threshold
  1  at least one vulnerability at or above the threshold, or without a score
  3  the scanner did not scan: invalid JSON, zero crates, empty database

Usage: audit-gate.py [--fail-on {critical,high,medium,low}] [--summary FILE] audit.json
"""

import argparse
import json
import math
import os
import sys

THRESHOLDS = {"low": 0.1, "medium": 4.0, "high": 7.0, "critical": 9.0}

# CVSS v3.x base metric weights (FIRST specification, section 7.4).
_AV = {"N": 0.85, "A": 0.62, "L": 0.55, "P": 0.2}
_AC = {"L": 0.77, "H": 0.44}
_UI = {"N": 0.85, "R": 0.62}
_CIA = {"H": 0.56, "L": 0.22, "N": 0.0}


def _roundup(value: float) -> float:
    scaled = int(round(value * 100000))
    if scaled % 10000 == 0:
        return scaled / 100000.0
    return (math.floor(scaled / 10000) + 1) / 10.0


def cvss3_score(vector: str):
    """Base score of a CVSS:3.0 or 3.1 vector, None for anything else.

    CVSS 4.0 needs a lookup table and is deliberately not scored: an unscored
    vulnerability is treated as blocking, which is the safe direction.
    """
    parts = vector.split("/")
    if not parts or parts[0] not in ("CVSS:3.0", "CVSS:3.1"):
        return None
    try:
        metrics = dict(p.split(":", 1) for p in parts[1:])
        scope_changed = metrics["S"] == "C"
        pr_table = {"N": 0.85, "L": 0.68 if scope_changed else 0.62, "H": 0.5 if scope_changed else 0.27}
        iss = 1 - (1 - _CIA[metrics["C"]]) * (1 - _CIA[metrics["I"]]) * (1 - _CIA[metrics["A"]])
        if scope_changed:
            impact = 7.52 * (iss - 0.029) - 3.25 * (iss - 0.02) ** 15
        else:
            impact = 6.42 * iss
        exploitability = 8.22 * _AV[metrics["AV"]] * _AC[metrics["AC"]] * pr_table[metrics["PR"]] * _UI[metrics["UI"]]
    except (KeyError, ValueError):
        return None
    if impact <= 0:
        return 0.0
    raw = impact + exploitability
    return _roundup(min(1.08 * raw, 10.0) if scope_changed else min(raw, 10.0))


def label(score) -> str:
    if score is None:
        return "unscored"
    for name in ("critical", "high", "medium", "low"):
        if score >= THRESHOLDS[name]:
            return name
    return "none"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("report")
    parser.add_argument("--fail-on", choices=sorted(THRESHOLDS), default="high")
    parser.add_argument("--summary", help="append a markdown summary to this file")
    args = parser.parse_args()

    try:
        with open(args.report, encoding="utf-8") as fh:
            report = json.load(fh)
        crates = int(report["lockfile"]["dependency-count"])
        advisories = int(report["database"]["advisory-count"])
    except (OSError, ValueError, KeyError, TypeError) as exc:
        print(f"audit-gate: broken scanner output ({exc})", file=sys.stderr)
        return 3
    if crates <= 0 or advisories <= 0:
        print(f"audit-gate: scanned {crates} crates against {advisories} advisories, nothing was checked", file=sys.stderr)
        return 3

    threshold = THRESHOLDS[args.fail_on]
    blocking, lower = [], []
    for item in report.get("vulnerabilities", {}).get("list", []):
        advisory, package = item["advisory"], item["package"]
        score = cvss3_score(advisory.get("cvss") or "")
        row = (advisory["id"], package["name"], package["version"], score, advisory.get("title", ""))
        (blocking if score is None or score >= threshold else lower).append(row)

    warnings = [
        (kind, w["advisory"]["id"], w["package"]["name"], w["package"]["version"], w["advisory"].get("title", ""))
        for kind, entries in report.get("warnings", {}).items()
        for w in entries
    ]

    lines = [
        f"cargo audit scanned {crates} crates against {advisories} advisories "
        f"(database commit {report['database'].get('last-commit', 'unknown')[:12]})",
        f"threshold: fail on {args.fail_on} and above (CVSS >= {threshold}) or unscored",
    ]
    for title, rows in (("BLOCKING", blocking), ("below threshold", lower)):
        for adv, name, version, score, text in rows:
            shown = "unscored" if score is None else f"{score} {label(score)}"
            lines.append(f"{title}: {adv} {name} {version} [{shown}] {text}")
    for kind, adv, name, version, text in warnings:
        lines.append(f"warning ({kind}): {adv} {name} {version} {text}")
    print("\n".join(lines))

    if args.summary:
        with open(args.summary, "a", encoding="utf-8") as fh:
            fh.write("### cargo audit\n\n```\n" + "\n".join(lines) + "\n```\n")
    if blocking and os.environ.get("GITHUB_ACTIONS"):
        for adv, name, version, _, _ in blocking:
            print(f"::error title=cargo audit::{adv} in {name} {version}")
    return 1 if blocking else 0


if __name__ == "__main__":
    sys.exit(main())
