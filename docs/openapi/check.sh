#!/usr/bin/env bash
# Validates docs/openapi/rustryx-api-v1.yaml.
#
#   1. OpenAPI 3.1 lint with Redocly CLI (pinned, fetched by npx; needs Node and network access).
#      Without npx the script falls back to openapi-spec-validator when it is installed.
#   2. House rules checked with the standard library only: every operation has an operationId,
#      tags, a summary, x-milestone, x-required-privilege (in the grammar of docs/api-conventions.md),
#      an explicit security list and at least one error response that resolves to the shared Error
#      envelope; mutable verbs follow the If-Match rule; secrets never appear in response schemas
#      except where the contract allows it; no em dash or en dash anywhere.
#
# Usage: docs/openapi/check.sh            (from anywhere)
#        REDOCLY_VERSION=1.34.5 docs/openapi/check.sh
#
# Exit code 0 means the contract is valid; any error or unexpected lint warning fails.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
spec="$here/rustryx-api-v1.yaml"
conventions="$here/../api-conventions.md"
redocly_version="${REDOCLY_VERSION:-1.34.5}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

command -v python3 >/dev/null 2>&1 || { echo "python3 is required" >&2; exit 2; }

# ---------------------------------------------------------------------------
# 1. Lint
# ---------------------------------------------------------------------------
if command -v npx >/dev/null 2>&1; then
  echo "== redocly lint (@redocly/cli@$redocly_version)"
  # Redocly exits non-zero on errors; the JSON report is inspected below either way.
  npx --yes "@redocly/cli@$redocly_version" lint "$spec" --format=json >"$tmp/lint.json" 2>"$tmp/lint.err" || true
  python3 - "$tmp/lint.json" <<'PY'
import json, sys

# Warnings that are correct and stay. Each entry is (rule, JSON pointer prefix, reason).
#   The two OIDC browser redirects answer 302 by design: the login start sends the browser to
#   Keycloak and the callback sends it back to the SPA, so neither has a 2XX response.
ALLOWED = [
    ("operation-2xx-response", "#/paths/~1api~1v1~1auth~1oidc~1login/get/responses", "browser redirect to the identity provider"),
    ("operation-2xx-response", "#/paths/~1api~1v1~1auth~1oidc~1callback/get/responses", "browser redirect back to the SPA"),
]

try:
    report = json.load(open(sys.argv[1]))
except ValueError:
    sys.exit("redocly did not produce a JSON report (is the network reachable for npx?)")

bad = []
for p in report.get("problems", []):
    pointer = p["location"][0].get("pointer", "") if p.get("location") else ""
    ok = p["severity"] == "warn" and any(p["ruleId"] == r and pointer.startswith(ptr) for r, ptr, _ in ALLOWED)
    if not ok:
        bad.append("%s %s %s %s" % (p["severity"], p["ruleId"], pointer, p["message"]))
totals = report.get("totals", {})
print("   errors: %s, warnings: %s (%d allowed)" % (totals.get("errors"), totals.get("warnings"), len(ALLOWED)))
if bad or totals.get("errors"):
    print("\n".join("   " + b for b in bad))
    sys.exit(1)
PY
elif python3 -c 'import openapi_spec_validator, yaml' >/dev/null 2>&1; then
  echo "== openapi-spec-validator (npx not found, no lint rules beyond the schema)"
  python3 -m openapi_spec_validator "$spec"
else
  echo "neither npx (Node) nor openapi-spec-validator is available; install one of them" >&2
  exit 2
fi

# ---------------------------------------------------------------------------
# 2. House rules
# ---------------------------------------------------------------------------
echo "== house rules"
if python3 -c 'import yaml' >/dev/null 2>&1; then
  python3 -c 'import json, sys, yaml; json.dump(yaml.safe_load(open(sys.argv[1])), open(sys.argv[2], "w"))' "$spec" "$tmp/spec.json"
elif command -v npx >/dev/null 2>&1; then
  npx --yes "@redocly/cli@$redocly_version" bundle "$spec" --ext json -o "$tmp/spec.json" >/dev/null 2>&1
else
  echo "need PyYAML or npx to read the specification" >&2
  exit 2
fi

python3 - "$tmp/spec.json" "$spec" "$conventions" <<'PY'
import json, re, sys

spec = json.load(open(sys.argv[1]))
files = [sys.argv[2], sys.argv[3]]
errors = []
comps = spec["components"]
METHODS = ("get", "put", "post", "delete", "patch", "head", "options")

# Grammar of x-required-privilege (docs/api-conventions.md, section Authorization).
TERM = (r"(anonymous|authenticated|interactive|nx-admin"
        r"|repo\.(browse|read|write|delete|admin)\((R|\*)\)( per item)?"
        r"|project\.(limited-guest|guest|developer|maintainer|project-admin)\(P\))")
PRIV = re.compile(r"^%s( or %s)*$" % (TERM, TERM))
MILESTONES = {"UI-M1", "UI-M2"}
# PUT operations that deliberately have no If-Match, with the reason.
NO_IF_MATCH = {
    "changeMyPassword": "own credential, not a shared object",
    "setAnonymousRepositories": "bulk set of flags, idempotent, all or nothing",
    "setComponentLabels": "replaces a label set, idempotent",
    "putOciTag": "moves a tag pointer, idempotent",
}


def resolve(node):
    seen = 0
    while isinstance(node, dict) and "$ref" in node and seen < 10:
        parts = node["$ref"].lstrip("#/").split("/")
        cur = spec
        for p in parts:
            cur = cur[p.replace("~1", "/").replace("~0", "~")]
        node = cur
        seen += 1
    return node


def refers_to_error(response):
    response = resolve(response)
    for mt in response.get("content", {}).values():
        s = mt.get("schema", {})
        if s.get("$ref") == "#/components/schemas/Error":
            return True
    return False


ids = {}
per_tag = {}
per_milestone = {}
count = 0
for path, item in spec["paths"].items():
    for method in METHODS:
        if method not in item:
            continue
        op = item[method]
        count += 1
        where = "%s %s" % (method.upper(), path)
        oid = op.get("operationId")
        if not oid:
            errors.append("%s: missing operationId" % where)
        elif oid in ids:
            errors.append("%s: operationId %s also used by %s" % (where, oid, ids[oid]))
        else:
            ids[oid] = where
        if not op.get("tags"):
            errors.append("%s: no tags" % where)
        if not op.get("summary"):
            errors.append("%s: no summary" % where)
        ms = op.get("x-milestone")
        if ms not in MILESTONES:
            errors.append("%s: x-milestone must be one of %s, got %r" % (where, sorted(MILESTONES), ms))
        priv = op.get("x-required-privilege")
        if not isinstance(priv, str) or not priv.strip():
            errors.append("%s: missing x-required-privilege" % where)
        elif not PRIV.match(priv):
            errors.append("%s: x-required-privilege %r is outside the grammar" % (where, priv))
        if "security" not in op:
            errors.append("%s: no explicit security list (use [] for public operations)" % where)
        responses = op.get("responses", {})
        errs = [code for code in responses if code[:1] in "45" and refers_to_error(responses[code])]
        if not errs:
            errors.append("%s: no 4xx or 5xx response referencing the Error envelope" % where)
        # Every non public operation documents 401 and 403; every operation documents 429 and 503.
        if op.get("security") != []:
            for need in ("401", "403"):
                if need not in responses:
                    errors.append("%s: missing %s response" % (where, need))
        for need in ("429", "503"):
            if need not in responses:
                errors.append("%s: missing %s response" % (where, need))
        # Optimistic concurrency: PUT and PATCH need If-Match, DELETE may carry it.
        pnames = [resolve(p).get("name") for p in op.get("parameters", [])]
        if method in ("put", "patch"):
            if "If-Match" not in pnames and oid not in NO_IF_MATCH:
                errors.append("%s: PUT and PATCH of an object need the If-Match parameter" % where)
            if "If-Match" in pnames and not ("412" in responses and "428" in responses):
                errors.append("%s: If-Match without 412 and 428 responses" % where)
        # 2xx exists, except the documented browser redirects.
        if not any(c[:1] == "2" for c in responses) and oid not in ("startOidcLogin", "completeOidcLogin"):
            errors.append("%s: no 2xx response" % where)
        tag = (op.get("tags") or ["?"])[0]
        per_tag.setdefault(tag, {"UI-M1": 0, "UI-M2": 0})
        if ms in MILESTONES:
            per_tag[tag][ms] += 1
            per_milestone[ms] = per_milestone.get(ms, 0) + 1

# Secrets in responses: only these properties may carry one, and only where the contract says so.
SECRET_NAME = re.compile(r"^(secret|password|passwd|private_key|api_key|access_key|secret_key|token|csrf_token|client_secret|bind_password)$")
ALLOWED_SECRETS = {
    ("Session", "csrf_token"), ("CsrfToken", "csrf_token"),
    ("ApiTokenCreated", "secret"), ("WebhookCreated", "secret"), ("MeView", "token"),
}
reachable = set()


def collect(node):
    if isinstance(node, dict):
        r = node.get("$ref")
        if isinstance(r, str) and r.startswith("#/components/schemas/"):
            name = r.rsplit("/", 1)[1]
            if name not in reachable:
                reachable.add(name)
                collect(comps["schemas"][name])
        for k, v in node.items():
            if k != "$ref":
                collect(v)
    elif isinstance(node, list):
        for v in node:
            collect(v)


for path, item in spec["paths"].items():
    for method in METHODS:
        if method in item:
            for code, resp in item[method]["responses"].items():
                if code[:1] == "2":
                    collect(resolve(resp))
for name in sorted(reachable):
    schema = comps["schemas"][name]
    stack = [schema]
    while stack:
        node = stack.pop()
        if isinstance(node, dict):
            for pname, pschema in (node.get("properties") or {}).items():
                if SECRET_NAME.match(pname) and (name, pname) not in ALLOWED_SECRETS and not (isinstance(pschema, dict) and pschema.get("writeOnly")):
                    errors.append("response schema %s exposes %r (allowed only for %s)" % (name, pname, sorted(ALLOWED_SECRETS)))
            stack.extend(v for k, v in node.items() if k != "properties" and isinstance(v, (dict, list)))
            stack.extend((node.get("properties") or {}).values())
        elif isinstance(node, list):
            stack.extend(node)

# Error catalogue: every code mentioned in the contract examples exists in x-known-codes.
known = {c["code"] for c in comps["schemas"]["ErrorBody"]["x-known-codes"]}
for name, r in comps["responses"].items():
    for ex in r.get("content", {}).get("application/json", {}).get("examples", {}).values():
        code = ex["value"]["error"]["code"]
        if code not in known:
            errors.append("response %s example uses code %s which is not in x-known-codes" % (name, code))

# docs/api-conventions.md matches the YAML: every operationId and every known error code appears in
# it (the operation index, the page map and the catalogue are generated from the contract).
try:
    doc = open(files[1], encoding="utf-8").read()
except OSError:
    doc = None
    errors.append("%s is missing" % files[1])
if doc is not None:
    for oid in sorted(ids):
        if ("`%s`" % oid) not in doc:
            errors.append("api-conventions.md does not mention operationId %s" % oid)
    for code in sorted(known):
        if ("`%s`" % code) not in doc:
            errors.append("api-conventions.md does not list error code %s" % code)
    index_ids = set(re.findall(r"^\| [^|]+ \| [A-Z]+ \| `/api/v1[^`]*` \| `([A-Za-z0-9]+)` \|", doc, re.M))
    for oid in sorted(index_ids - set(ids)):
        errors.append("api-conventions.md index names operationId %s which is not in the YAML" % oid)
    for oid in sorted(set(ids) - index_ids):
        errors.append("api-conventions.md operation index misses %s" % oid)

# No em dash and no en dash in the contract files.
for f in files:
    try:
        text = open(f, encoding="utf-8").read()
    except OSError:
        continue
    for ch, nm in (("\u2014", "em dash"), ("\u2013", "en dash")):
        if ch in text:
            errors.append("%s contains an %s" % (f, nm))

if errors:
    print("   %d problem(s):" % len(errors))
    for e in errors:
        print("   - " + e)
    sys.exit(1)

print("   %d operations, %d schemas; every operation has operationId, tags, summary, x-milestone," % (count, len(comps["schemas"])))
print("   x-required-privilege, security and an Error response")
print("   per milestone: " + ", ".join("%s=%d" % (k, v) for k, v in sorted(per_milestone.items())))
print("   per tag:")
for tag, c in sorted(per_tag.items()):
    print("     %-22s M1=%-3d M2=%d" % (tag, c["UI-M1"], c["UI-M2"]))
PY

# ---------------------------------------------------------------------------
# 3. Optional: every example in the document validates against its schema
# ---------------------------------------------------------------------------
if python3 -c 'import jsonschema, yaml' >/dev/null 2>&1; then
  echo "== examples against schemas (jsonschema)"
  python3 - "$spec" <<'PY'
import sys, yaml
from jsonschema import Draft202012Validator

spec = yaml.safe_load(open(sys.argv[1]))
comps = spec["components"]
bad = 0


def check(schema, example, where):
    global bad
    doc = {"$schema": "https://json-schema.org/draft/2020-12/schema", "components": comps}
    doc.update(schema)
    for e in Draft202012Validator(doc).iter_errors(example):
        bad += 1
        print("   %s: %s %s" % (where, "/".join(str(p) for p in e.path), e.message[:160]))


for name, s in comps["schemas"].items():
    if "example" in s:
        check({"$ref": "#/components/schemas/" + name}, s["example"], "schema " + name)
for path, item in spec["paths"].items():
    for method, op in item.items():
        for ct, mt in op.get("requestBody", {}).get("content", {}).items():
            if ct.startswith("multipart"):
                continue
            for k, v in ([("example", mt["example"])] if "example" in mt else []) + [(k, v["value"]) for k, v in mt.get("examples", {}).items()]:
                check(mt["schema"], v, "%s %s request %s" % (method, path, k))
        for code, r in op["responses"].items():
            if "$ref" in r:
                continue
            for ct, mt in r.get("content", {}).items():
                if ct != "application/json":
                    continue
                for k, v in ([("example", mt["example"])] if "example" in mt else []) + [(k, v["value"]) for k, v in mt.get("examples", {}).items()]:
                    check(mt["schema"], v, "%s %s %s %s" % (method, path, code, k))
sys.exit(1 if bad else 0)
PY
  echo "   all examples valid"
else
  echo "== examples against schemas: skipped (pip install jsonschema pyyaml to enable)"
fi

echo "OK"
