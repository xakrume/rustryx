#!/usr/bin/env python3
"""Check that every supply-chain exception is recorded, justified and not expired.

Policy: docs/supply-chain.md, section "Exceptions". The checks below are the
negative requirements of that policy, so each has a test in
ci/tests/test_check_exceptions.py:

  1. an advisory ignore carries `issue=<ref>; until=YYYY-MM-DD; <reason>`;
  2. `until` is not in the past and not more than MAX_DAYS ahead;
  3. deny.toml and .cargo/audit.toml ignore exactly the same advisories;
  4. the licence allowlist is exactly the one of README 8.6, and no per-crate
     exception allows GPL, LGPL or AGPL;
  5. every per-crate licence exception and every duplicate skip has a reason;
  6. only crates.io is a source, git and unknown registries stay denied.

Usage: ci/check-exceptions.py [--deny deny.toml] [--audit .cargo/audit.toml] [--today YYYY-MM-DD]
Exit code 0 when clean, 1 with one line per violation.
"""

import argparse
import datetime
import re
import sys
import tomllib

# README 8.6. Changing this list is a decision of the maintainers, so it is
# written down twice (here and in deny.toml) and a difference fails the check.
ALLOWED_LICENCES = frozenset(
    {
        "MIT",
        "Apache-2.0",
        "BSD-2-Clause",
        "BSD-3-Clause",
        "ISC",
        "Zlib",
        "Unicode-3.0",
        "Unicode-DFS-2016",
        "MPL-2.0",
    }
)
CRATES_IO_INDEX = "https://github.com/rust-lang/crates.io-index"
MAX_DAYS = 180
RECORD = re.compile(r"^issue=(?P<issue>\S+); until=(?P<until>\d{4}-\d{2}-\d{2}); (?P<why>.{20,})$", re.DOTALL)
COPYLEFT = re.compile(r"(^|[^A-Za-z])(A|L)?GPL", re.IGNORECASE)


def _ignore_entries(deny: dict) -> list:
    return deny.get("advisories", {}).get("ignore", [])


def check_advisory_records(deny: dict, today: datetime.date) -> list:
    problems = []
    for entry in _ignore_entries(deny):
        if not isinstance(entry, dict) or "id" not in entry:
            problems.append(f"advisories.ignore entry {entry!r} must be a table with id and reason")
            continue
        ident = entry["id"]
        match = RECORD.match(entry.get("reason", ""))
        if not match:
            problems.append(f"{ident}: reason must match 'issue=<ref>; until=YYYY-MM-DD; <why, 20+ chars>'")
            continue
        try:
            until = datetime.date.fromisoformat(match["until"])
        except ValueError:
            problems.append(f"{ident}: until={match['until']} is not a date")
            continue
        if until < today:
            problems.append(f"{ident}: exception expired on {until}; fix the dependency or renew with a new issue")
        elif (until - today).days > MAX_DAYS:
            problems.append(f"{ident}: until={until} is more than {MAX_DAYS} days ahead")
    return problems


def check_ignore_sync(deny: dict, audit: dict) -> list:
    in_deny = {e["id"] if isinstance(e, dict) else e for e in _ignore_entries(deny)}
    in_audit = set(audit.get("advisories", {}).get("ignore", []))
    problems = []
    for ident in sorted(in_deny - in_audit):
        problems.append(f"{ident}: ignored in deny.toml but not in .cargo/audit.toml")
    for ident in sorted(in_audit - in_deny):
        problems.append(f"{ident}: ignored in .cargo/audit.toml but not in deny.toml")
    return problems


def check_licences(deny: dict, raw: str) -> list:
    problems = []
    licences = deny.get("licenses", {})
    allowed = set(licences.get("allow", []))
    for extra in sorted(allowed - ALLOWED_LICENCES):
        problems.append(f"licenses.allow contains {extra}, not in the README 8.6 allowlist")
    for missing in sorted(ALLOWED_LICENCES - allowed):
        problems.append(f"licenses.allow lacks {missing}, required by README 8.6")
    for exc in licences.get("exceptions", []):
        crate = exc.get("crate", "?")
        for lic in exc.get("allow", []):
            if COPYLEFT.search(lic):
                problems.append(f"licence exception for {crate} allows copyleft licence {lic}")
        if not re.search(rf"^\s*#\s*exception {re.escape(crate.split('@')[0])}: \S", raw, re.MULTILINE):
            problems.append(f"licence exception for {crate} has no '# exception {crate}: <reason>' comment")
    return problems


def check_skips(deny: dict) -> list:
    problems = []
    for skip in deny.get("bans", {}).get("skip", []):
        if not isinstance(skip, dict) or not skip.get("reason", "").strip():
            problems.append(f"bans.skip entry {skip!r} has no reason")
    return problems


def check_sources(deny: dict) -> list:
    sources = deny.get("sources", {})
    problems = []
    if sources.get("unknown-registry") != "deny":
        problems.append("sources.unknown-registry must be deny")
    if sources.get("unknown-git") != "deny":
        problems.append("sources.unknown-git must be deny")
    if sources.get("allow-registry") != [CRATES_IO_INDEX]:
        problems.append("sources.allow-registry must be exactly the crates.io index")
    if sources.get("allow-git"):
        problems.append("sources.allow-git must stay empty; a git dependency needs a maintainer decision")
    return problems


def run(deny_path: str, audit_path: str, today: datetime.date) -> list:
    with open(deny_path, "rb") as fh:
        raw_bytes = fh.read()
    deny = tomllib.loads(raw_bytes.decode("utf-8"))
    with open(audit_path, "rb") as fh:
        audit = tomllib.load(fh)
    return (
        check_advisory_records(deny, today)
        + check_ignore_sync(deny, audit)
        + check_licences(deny, raw_bytes.decode("utf-8"))
        + check_skips(deny)
        + check_sources(deny)
    )


def main(argv: list) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--deny", default="deny.toml")
    parser.add_argument("--audit", default=".cargo/audit.toml")
    parser.add_argument("--today", default=None, help="override the date, for tests")
    args = parser.parse_args(argv)
    today = datetime.date.fromisoformat(args.today) if args.today else datetime.date.today()
    problems = run(args.deny, args.audit, today)
    for line in problems:
        print(f"check-exceptions: {line}", file=sys.stderr)
    if not problems:
        print(f"check-exceptions: ok ({today})")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
