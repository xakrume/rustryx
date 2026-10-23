#!/usr/bin/env python3
"""Static checks for the monitoring assets. Run by deploy/helm/test.sh.

Each check pins a rule from the spec that a green `promtool check rules` does not cover:

  * SRE-10: no rate() on counters that see fewer than one event per minute.
  * SRE-13: the dashboard has the required sections.
  * Every alert has severity, summary and a runbook_url whose anchor exists in docs/runbook.md.
  * Every metric a rule or a panel queries is in the catalogue of docs/slo.md, and the catalogue is
    honest about which of them the code exports: a metric the code exports must not be listed as
    pending, a metric listed as exported must really be in rustryx-observability's names.rs.
    A rule on a series nobody emits is an alert that cannot fire, so the gap has to be visible.
  * Metric names are generic (http_requests_total), so every selector carries job="rustryx".
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MON = ROOT / "deploy" / "monitoring"
NAMES_RS = ROOT / "crates" / "rustryx-observability" / "src" / "names.rs"
errors: list[str] = []

# Counters with a steady, high event rate. Anything else must use increase().
HIGH_FREQUENCY = {
    "http_requests_total",
    "http_request_duration_seconds_bucket",
    "upstream_requests_total",
    "upstream_request_duration_seconds_bucket",
    "blob_store_ops_total",
    "blob_store_bytes_total",
    "db_pool_wait_seconds_bucket",
    "redis_requests_total",
    "process_cpu_seconds_total",
}
REQUIRED_SECTIONS = [
    "Golden signals", "Upstream", "Blob store", "PostgreSQL pool", "Background jobs", "Authentication", "Runtime",
]
# Series that are not Rustryx application metrics: scraper, cAdvisor, kube-state-metrics, process.
FOREIGN = re.compile(r"^(up|process_.*|container_.*|kube_.*|certmanager_.*)$")


def slug(heading: str) -> str:
    """GitHub anchor of a heading."""
    return re.sub(r"[^a-z0-9 _-]", "", heading.strip().lower()).replace(" ", "-")


def section(text: str, heading: str) -> str:
    """Body of the markdown section whose heading line contains `heading`."""
    lines = text.splitlines()
    start = next((i for i, line in enumerate(lines) if line.startswith("#") and heading in line), None)
    if start is None:
        return ""
    level = len(lines[start]) - len(lines[start].lstrip("#"))
    body = []
    for line in lines[start + 1:]:
        if line.startswith("#") and len(line) - len(line.lstrip("#")) <= level:
            break
        body.append(line)
    return "\n".join(body)


dash = json.loads((MON / "rustryx-dashboard.json").read_text())
exprs: list[tuple[str, str]] = []
rows: list[str] = []
for p in dash["panels"]:
    if p.get("type") == "row":
        rows.append(p["title"])
        continue
    if not p.get("targets"):
        errors.append(f"panel '{p.get('title')}' has no targets")
    for t in p.get("targets", []):
        exprs.append((p.get("title", "?"), t["expr"]))
for need in REQUIRED_SECTIONS:
    if not any(need.lower() in r.lower() for r in rows):
        errors.append(f"dashboard misses the section '{need}'")

# Layout: unique panel ids, panels inside the 24 column grid, no overlap within a row of the grid.
ids = [p["id"] for p in dash["panels"]]
if len(ids) != len(set(ids)):
    errors.append("dashboard panel ids are not unique")
by_y: dict[int, list[tuple[int, int, str]]] = {}
for p in dash["panels"]:
    g = p["gridPos"]
    if g["x"] < 0 or g["x"] + g["w"] > 24:
        errors.append(f"panel '{p['title']}' leaves the 24 column grid")
    by_y.setdefault(g["y"], []).append((g["x"], g["x"] + g["w"], p["title"]))
for y, items in by_y.items():
    items.sort()
    for left, right in zip(items, items[1:]):
        if left[1] > right[0]:
            errors.append(f"panels '{left[2]}' and '{right[2]}' overlap at y={y}")

# `--emit-promtool FILE`: write every panel query as a recording rule, Grafana macros replaced, so
# that `promtool check rules` proves each query parses. A panel with a syntax error is empty in Grafana.
if len(sys.argv) == 3 and sys.argv[1] == "--emit-promtool":
    lines = ["groups:", "  - name: dashboard", "    rules:"]
    for p in dash["panels"]:
        for i, t in enumerate(p.get("targets", [])):
            expr = re.sub(r"\$__rate_interval|\$__interval", "5m", t["expr"]).replace("$namespace", ".+")
            lines += [f"      - record: dashboard_{p['id']}_{i}", f"        expr: {json.dumps(expr)}"]
    Path(sys.argv[2]).write_text("\n".join(lines) + "\n")

rules_text = (MON / "rustryx-alerts.yaml").read_text()
rule_exprs = re.findall(r"(?m)^\s+expr: (?:\|\n((?:\s{10,}.*\n)+)|(.*)\n)", rules_text)
for block, inline in rule_exprs:
    exprs.append(("rule", block or inline))

rate_re = re.compile(r"\b(?:irate|rate)\(\s*([A-Za-z_:][A-Za-z0-9_:]*)")
for title, expr in exprs:
    for metric in rate_re.findall(expr):
        if metric not in HIGH_FREQUENCY and not metric.startswith("rustryx:"):
            errors.append(f"'{title}': rate() on rare-event metric {metric}; use increase() (SRE-10)")

# ---- runbook anchors and required annotations
runbook = ROOT / "docs" / "runbook.md"
anchors: set[str] = set()
if runbook.exists():
    for line in runbook.read_text().splitlines():
        m = re.match(r"^#{2,4}\s+(.*)$", line)
        if m:
            anchors.add(slug(m.group(1)))
else:
    errors.append("docs/runbook.md is missing")
for alert, block in re.findall(r"- alert: (\w+)\n((?:        .*\n|      .*\n)+)", rules_text):
    for key in ("severity:", "summary:", "runbook_url:"):
        if key not in block:
            errors.append(f"alert {alert} lacks {key[:-1]}")
    m = re.search(r"runbook_url: \S+#(\S+)", block)
    if m and runbook.exists() and m.group(1) not in anchors:
        errors.append(f"alert {alert}: runbook anchor #{m.group(1)} not found in docs/runbook.md")
    sev = re.search(r"severity: (\w+)", block)
    if sev and sev.group(1) not in ("critical", "high", "medium"):
        errors.append(f"alert {alert}: unknown severity {sev.group(1)}")

# ---- metric catalogue
exported: set[str] = set()
if NAMES_RS.exists():
    exported = set(re.findall(r'pub const [A-Z0-9_]+: &str =\s*"([a-z0-9_]+)";', NAMES_RS.read_text()))
else:
    print(f"note: {NAMES_RS.relative_to(ROOT)} not found, skipping the exported-versus-listed check")

slo = ROOT / "docs" / "slo.md"
if not slo.exists():
    errors.append("docs/slo.md is missing")
else:
    doc = slo.read_text()
    # The first column of the catalogue tables names the series; backticks in prose do not count.
    first_col = re.compile(r"(?m)^\|\s*`([a-z][a-z0-9_]*)`\s*\|")
    pending = set(first_col.findall(section(doc, "Not yet exported")))
    listed = set(first_col.findall(section(doc, "Metric catalogue")))
    # Metrics that the assets query, found by the job="rustryx" selector every one of them carries.
    used: set[str] = set()
    for _, expr in exprs:
        for name in re.findall(r'([a-z_][a-z0-9_]*)\{job="rustryx"', expr):
            used.add(re.sub(r"_bucket$", "", name))
    used = {n for n in used if not FOREIGN.match(n)}
    for name in sorted(used):
        if name not in listed and name not in pending:
            errors.append(f"metric {name} is queried but listed neither as exported nor as pending in docs/slo.md")
        if exported and name not in exported and name not in pending:
            errors.append(f"metric {name} is queried, not exported by names.rs and not marked pending in docs/slo.md")
        if name in exported and name in pending:
            errors.append(f"metric {name} is exported by names.rs but docs/slo.md still lists it as pending")
    for name in sorted(listed & used):
        if exported and name not in exported and name not in pending:
            errors.append(f"docs/slo.md lists {name} as exported but names.rs does not define it")
    # Every application metric must be selected with job="rustryx": names are generic.
    known = (exported | pending | used) - {"up"}
    for title, expr in exprs:
        for name in known:
            if FOREIGN.match(name):
                continue
            bare = re.search(rf'(?<![:\w]){re.escape(name)}(?:_bucket)?(?![:\w])(?!\{{job="rustryx")', expr)
            if bare and not re.search(rf"rustryx:[a-z0-9_:]*{re.escape(name)}", expr):
                errors.append(f"'{title}': {name} is selected without job=\"rustryx\"")

if errors:
    print("monitoring asset check FAILED:")
    for e in errors:
        print(f"  - {e}")
    sys.exit(1)
print(f"monitoring assets ok: {len(exprs)} queries, {len(rows)} dashboard sections")
