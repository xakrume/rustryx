#!/usr/bin/env python3
"""Judge a Trivy JSON report (`trivy ... --format json --list-all-pkgs`).

The scanner itself is run with `--exit-code 0` and its real exit status is passed
in with --scanner-exit-code, so that a crashed or half-initialised scanner can
never be mistaken for a clean image (DSO-00, DSO-00b, OPS-25).

Exit codes, one meaning each:
  0  clean
  1  blocking: a fixable CRITICAL (always, in both modes), or, with --enforce,
     a fixable finding at or above --block-severity
  2  findings below the blocking line, reported but not failing in observation
     mode. The workflow turns this into a warning unless SECURITY_GATES_ENFORCE
     is "true", in which case this script already exits 1 for them.
  3  the scanner did not scan: it exited non-zero, or the report lists fewer than
     --min-components packages

"Fixable" means Trivy knows a fixed version. Unfixed findings are counted and
printed but do not block, because nothing can be done about them but an
exception; use --include-unfixed to block on them as well.
Usage: check-trivy.py [options] trivy.json
"""

import argparse
import json
import os
import sys

ORDER = ["UNKNOWN", "LOW", "MEDIUM", "HIGH", "CRITICAL"]


def load(path: str) -> dict:
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def summarise(report: dict) -> tuple:
    components, findings = 0, []
    for result in report.get("Results") or []:
        components += len(result.get("Packages") or [])
        for vuln in result.get("Vulnerabilities") or []:
            findings.append({
                "id": vuln.get("VulnerabilityID", "?"),
                "pkg": vuln.get("PkgName", "?"),
                "installed": vuln.get("InstalledVersion", "?"),
                "fixed": vuln.get("FixedVersion") or "",
                "severity": vuln.get("Severity", "UNKNOWN").upper(),
                "target": result.get("Target", "?"),
            })
    return components, findings


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("report")
    parser.add_argument("--label", default="trivy")
    parser.add_argument("--scanner-exit-code", type=int, default=0)
    parser.add_argument("--min-components", type=int, default=1)
    parser.add_argument("--enforce", choices=["true", "false"], default="false")
    parser.add_argument("--block-severity", choices=ORDER[1:], default="HIGH")
    parser.add_argument("--include-unfixed", action="store_true")
    parser.add_argument("--summary", help="append a markdown summary to this file")
    args = parser.parse_args()

    try:
        components, findings = summarise(load(args.report))
    except (OSError, ValueError, AttributeError) as exc:
        print(f"check-trivy[{args.label}]: unreadable report {args.report}: {exc}", file=sys.stderr)
        return 3

    counted = [f for f in findings if args.include_unfixed or f["fixed"]]
    by_sev = {s: sum(1 for f in counted if f["severity"] == s) for s in reversed(ORDER)}
    unfixed = sum(1 for f in findings if not f["fixed"])
    lines = [
        f"{args.label}: scanner exit code {args.scanner_exit_code}, {components} packages scanned, "
        f"{len(findings)} findings ({unfixed} without a fix)",
        f"{args.label}: counted " + ", ".join(f"{s} {n}" for s, n in by_sev.items()),
    ]
    for f in sorted(counted, key=lambda f: -ORDER.index(f["severity"]))[:25]:
        lines.append(f"  {f['severity']:<8} {f['id']} {f['pkg']} {f['installed']} -> {f['fixed'] or 'no fix'} ({f['target']})")
    print("\n".join(lines))
    if args.summary:
        with open(args.summary, "a", encoding="utf-8") as fh:
            fh.write(f"### {args.label}\n\n```\n" + "\n".join(lines) + "\n```\n")

    if args.scanner_exit_code != 0:
        print(f"check-trivy[{args.label}]: the scanner itself failed (exit {args.scanner_exit_code})", file=sys.stderr)
        return 3
    if components < args.min_components:
        print(f"check-trivy[{args.label}]: {components} packages scanned, at least {args.min_components} expected: "
              "the scan covered nothing", file=sys.stderr)
        return 3
    if by_sev["CRITICAL"]:
        return 1
    block_from = ORDER.index(args.block_severity)
    at_or_above = sum(n for s, n in by_sev.items() if ORDER.index(s) >= block_from)
    below = sum(n for s, n in by_sev.items() if ORDER.index(s) < block_from)
    if args.enforce == "true" and at_or_above:
        return 1
    if at_or_above or below:
        if os.environ.get("GITHUB_ACTIONS"):
            print(f"::warning title={args.label}::findings below the blocking line "
                  f"(set SECURITY_GATES_ENFORCE to true to block)")
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
