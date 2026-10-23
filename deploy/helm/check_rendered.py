#!/usr/bin/env python3
"""Invariants every rendered manifest set must hold, whatever values produced it.

Reads a multi-document YAML stream (output of `helm template`) on stdin and exits non-zero with a
list of violations. The rules mirror the spec so a matrix case cannot pass lint and kubeconform
while breaking one of them:

  OPS-11  hardened container (non-root 10001, no escalation, read-only rootfs, drop ALL, seccomp)
  OPS-18  no `latest` tag, every image has a tag or a digest
  OPS-33  probe port equals a declared containerPort, one block per probe, liveness is /healthz
  OPS-36  no API token mounted, no Role, ClusterRole or binding rendered
  OPS-39  the chart renders no Secret, no literal secret in env, no VAULT_* variable
  OPS-31  rolling update never lowers capacity (maxUnavailable 0) unless the strategy is Recreate
"""
import re
import sys
from typing import Any, Optional

try:
    import yaml
except ImportError:  # pragma: no cover
    sys.exit("PyYAML is required: pip install pyyaml")

SECRET_NAME = re.compile(r"PASSWORD|SECRET|TOKEN|PRIVATE|SIGNING|SESSION_KEY|ENCRYPTION_KEY|ACCESS_KEY|DATABASE_URL|DSN", re.I)
errors = []
docs = [d for d in yaml.safe_load_all(sys.stdin) if d]


def err(doc: dict[str, Any], msg: str) -> None:
    errors.append(f"{doc.get('kind')}/{doc.get('metadata', {}).get('name')}: {msg}")


def pod_spec(doc: dict[str, Any]) -> Optional[dict[str, Any]]:
    """Pod spec of the workloads the chart renders, None for every other kind."""
    if doc["kind"] in ("Deployment", "Job"):
        return doc["spec"]["template"]["spec"]
    return None


for doc in docs:
    kind = doc["kind"]
    if kind == "Secret":
        err(doc, "the chart must not render a Secret (OPS-39)")
    if kind in ("Role", "ClusterRole", "RoleBinding", "ClusterRoleBinding"):
        err(doc, "the chart must not render RBAC (OPS-36)")

    spec = pod_spec(doc)
    if spec is None:
        continue
    if spec.get("automountServiceAccountToken") is not False:
        err(doc, "automountServiceAccountToken must be false")
    if spec.get("enableServiceLinks") is not False:
        err(doc, "enableServiceLinks must be false (RUSTRYX_SERVICE_* variables are noise and a trap)")
    psc = spec.get("securityContext", {})
    if psc.get("runAsNonRoot") is not True:
        err(doc, "pod runAsNonRoot must be true")
    if psc.get("seccompProfile", {}).get("type") != "RuntimeDefault":
        err(doc, "pod seccompProfile must be RuntimeDefault")

    containers = spec.get("containers", [])
    for c in containers:
        where = f"container {c['name']}"
        sc = c.get("securityContext", {})
        if sc.get("runAsNonRoot") is not True or sc.get("runAsUser") != 10001 or sc.get("runAsGroup") != 10001:
            err(doc, f"{where}: must run as 10001:10001 non-root")
        if sc.get("allowPrivilegeEscalation") is not False:
            err(doc, f"{where}: allowPrivilegeEscalation must be false")
        if sc.get("readOnlyRootFilesystem") is not True:
            err(doc, f"{where}: readOnlyRootFilesystem must be true")
        if sc.get("capabilities", {}).get("drop") != ["ALL"] or "add" in sc.get("capabilities", {}):
            err(doc, f"{where}: capabilities must drop ALL and add nothing")
        if sc.get("seccompProfile", {}).get("type") != "RuntimeDefault":
            err(doc, f"{where}: seccompProfile must be RuntimeDefault")
        if sc.get("privileged"):
            err(doc, f"{where}: must not be privileged")

        image = c.get("image", "")
        last = image.split("/")[-1]
        if "@sha256:" not in image and ":" not in last:
            err(doc, f"{where}: image {image} has neither tag nor digest")
        if re.search(r":latest$", image, re.I):
            err(doc, f"{where}: image uses latest (OPS-18)")

        for e in c.get("env", []):
            name = e["name"]
            if name.startswith("VAULT_") or name.endswith(("ROLE_ID", "SECRET_ID")):
                err(doc, f"{where}: {name} must not exist, the pod holds no Vault client (OPS-39)")
            if SECRET_NAME.search(name) and "value" in e and not name.endswith("_FILE"):
                err(doc, f"{where}: {name} carries a literal value (OPS-39)")
            if name.endswith("_FILE") and not e.get("value", "").startswith("/"):
                err(doc, f"{where}: {name} must point to an absolute path")
        for ef in c.get("envFrom", []):
            if "secretRef" not in ef and "configMapRef" not in ef:
                err(doc, f"{where}: unknown envFrom entry")

    if kind != "Deployment":
        continue

    # Probes (OPS-33). One container, each probe exactly once, port equals a declared containerPort.
    if len(containers) != 1:
        err(doc, f"expected one container, found {len(containers)}")
        continue
    c = containers[0]
    ports = {p["name"]: p["containerPort"] for p in c.get("ports", [])}
    # One listener serves everything unless a dedicated ops listener is declared.
    ops = ports.get("ops", ports.get("http"))
    for probe in ("startupProbe", "readinessProbe", "livenessProbe"):
        p = c.get(probe)
        if p is None:
            if probe != "startupProbe":
                err(doc, f"{probe} is missing")
            continue
        port = p["httpGet"]["port"]
        if port != ops or port not in ports.values():
            err(doc, f"{probe} port {port} is not the listener containerPort {ops}")
        if "initialDelaySeconds" in p and c.get("startupProbe"):
            err(doc, f"{probe} sets initialDelaySeconds next to a startupProbe")
    live = c.get("livenessProbe", {}).get("httpGet", {}).get("path")
    ready = c.get("readinessProbe", {}).get("httpGet", {}).get("path")
    start = c.get("startupProbe", {}).get("httpGet", {}).get("path", "/healthz")
    if live != "/healthz" or start != "/healthz":
        err(doc, f"liveness and startup must use /healthz, got {live} and {start}")
    if ready != "/readyz":
        err(doc, f"readiness must use /readyz, got {ready}")

    strategy = doc["spec"].get("strategy", {})
    if strategy.get("type") == "RollingUpdate" and strategy.get("rollingUpdate", {}).get("maxUnavailable") != 0:
        err(doc, "RollingUpdate must keep maxUnavailable at 0")

    terminate = spec.get("terminationGracePeriodSeconds", 30)
    sleep = c.get("lifecycle", {}).get("preStop", {}).get("sleep", {}).get("seconds", 0)
    if sleep <= 0:
        err(doc, "preStop sleep is missing: the balancer would still route to a stopping pod")
    if terminate <= sleep:
        err(doc, f"terminationGracePeriodSeconds {terminate} leaves no time to drain after preStop {sleep}")


if errors:
    print("rendered manifest invariants FAILED:")
    for e in errors:
        print(f"  - {e}")
    sys.exit(1)
