#!/usr/bin/env bash
# Runs the whole chart test matrix: lint, render, kubeconform, manifest invariants, negative cases,
# helm unittest, monitoring asset checks and static hygiene checks. CI calls this script; it exits
# non-zero if anything failed, after running everything (one run shows every problem).
#
# Needs: helm (3.14+ or 4), the helm-unittest plugin, kubeconform, promtool, jq, python3 with PyYAML.
# Network: kubeconform downloads the Kubernetes and CRD JSON schemas (cached in SCHEMA_CACHE).
#
# Environment:
#   K8S_VERSION        Kubernetes version the manifests are validated against (default 1.32.0)
#   SCHEMA_CACHE       kubeconform schema cache directory
#   SKIP_KUBECONFORM=1 skip schema validation (offline runs)
#   SKIP_UNITTEST=1    skip helm unittest
#   MANIFEST_SCANNER   manifest scanner command taking a manifest file as last argument; default kube-linter if installed
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
chart="$here/rustryx"
repo="$(cd "$here/../.." && pwd)"
mon="$repo/deploy/monitoring"
K8S_VERSION="${K8S_VERSION:-1.32.0}"
SCHEMA_CACHE="${SCHEMA_CACHE:-${TMPDIR:-/tmp}/rustryx-kubeconform-cache}"
# Not a real image: a digest-shaped placeholder so values-prod.yaml (requireDigest) can render.
DIGEST="sha256:0000000000000000000000000000000000000000000000000000000000000000"
CRD_SCHEMAS='https://raw.githubusercontent.com/datreeio/CRDs-catalog/main/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json'

failures=()
passes=0
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

ok() { passes=$((passes + 1)); printf '  ok    %s\n' "$1"; }
bad() { failures+=("$1"); printf '  FAIL  %s\n' "$1"; if [ -n "${2:-}" ]; then printf '%s\n' "$2" | sed 's/^/        /'; fi; }
section() { printf '\n== %s\n' "$1"; }

need() {
  command -v "$1" >/dev/null 2>&1 || { echo "missing tool: $1 ($2)" >&2; exit 2; }
}
need helm "https://helm.sh/docs/intro/install/"
need jq "brew install jq"
need python3 "python 3.9+"
need promtool "brew install prometheus, or download promtool from the Prometheus release"
python3 -I -c 'import yaml' 2>/dev/null || { echo "missing PyYAML: pip install pyyaml" >&2; exit 2; }
helm plugin list 2>/dev/null | grep -q '^unittest' || [ "${SKIP_UNITTEST:-0}" = 1 ] || {
  echo "missing helm plugin: helm plugin install https://github.com/helm-unittest/helm-unittest --verify=false" >&2; exit 2; }
[ "${SKIP_KUBECONFORM:-0}" = 1 ] || need kubeconform "brew install kubeconform"

# --------------------------------------------------------------------------------------------
# The matrix. Each case is layered over ci/base-values.yaml unless it names its own files.
# Covers: S3 and local storage, Ingress and Gateway, subdomain connector, autoscaling on and off,
# monitoring on and off (both flavors), everything on, and the two environment files.
# --------------------------------------------------------------------------------------------
ci="$chart/ci"
declare -a case_names case_args
add_case() { case_names+=("$1"); shift; case_args+=("$*"); }
b="-f $ci/base-values.yaml"
add_case "s3 (ingress off, gateway off, hpa off, monitoring off)" "$b"
add_case "s3 + ingress"                          "$b -f $ci/ingress-values.yaml"
add_case "s3 + gateway"                          "$b -f $ci/gateway-values.yaml"
add_case "local single replica (Recreate)"       "$b -f $ci/local-single-values.yaml"
add_case "local RWX confirmed, 2 replicas"       "$b -f $ci/local-rwx-values.yaml"
add_case "local RWX + ingress"                   "$b -f $ci/local-rwx-values.yaml -f $ci/ingress-values.yaml"
add_case "subdomain connector (ingress)"         "$b -f $ci/subdomain-connector-values.yaml"
add_case "subdomain connector (gateway)"         "$b -f $ci/gateway-values.yaml -f $ci/subdomain-connector-values.yaml --set ingress.enabled=false"
add_case "autoscaling on"                        "$b -f $ci/autoscaling-values.yaml"
add_case "autoscaling on + ingress"              "$b -f $ci/autoscaling-values.yaml -f $ci/ingress-values.yaml"
add_case "monitoring on (prometheus operator)"   "$b -f $ci/monitoring-prometheus-values.yaml"
add_case "monitoring on (victoriametrics)"       "$b -f $ci/monitoring-vm-values.yaml"
add_case "autoscaling + monitoring + ingress"    "$b -f $ci/autoscaling-values.yaml -f $ci/monitoring-prometheus-values.yaml -f $ci/ingress-values.yaml"
add_case "everything on (OIDC, LDAP, Redis, file secrets, ESO, CA, Cilium)" "$b -f $ci/full-values.yaml"
add_case "values-test.yaml"                      "-f $chart/values-test.yaml"
add_case "values-prod.yaml (pinned by digest)"   "-f $chart/values-prod.yaml --set image.digest=$DIGEST"

render() { # render <helm args...>: manifests on stdout; on failure helm's error text on stdout, status kept
  local err rc=0
  err="$(mktemp)"
  helm template rustryx "$chart" --namespace rustryx --kube-version "$K8S_VERSION" "$@" 2>"$err" || rc=$?
  if [ "$rc" -ne 0 ]; then cat "$err"; fi
  rm -f "$err"
  return "$rc"
}

section "helm lint --strict and helm template"
for i in "${!case_names[@]}"; do
  name="${case_names[$i]}"
  # shellcheck disable=SC2086
  if out="$(helm lint "$chart" --strict ${case_args[$i]} 2>&1 | grep -v -E 'funcMap|icon is recommended')" \
     && ! grep -q '\[ERROR\]\|\[WARNING\]' <<<"$out"; then ok "lint: $name"; else bad "lint: $name" "$out"; fi
  # shellcheck disable=SC2086
  if render ${case_args[$i]} > "$work/case-$i.yaml"; then
    [ -s "$work/case-$i.yaml" ] && ok "template: $name" || bad "template: $name (empty output)"
  else
    bad "template: $name" "$(cat "$work/case-$i.yaml")"
  fi
done

if [ "${SKIP_KUBECONFORM:-0}" != 1 ]; then
  section "kubeconform (strict, Kubernetes $K8S_VERSION, CRD schemas for ServiceMonitor, VMServiceScrape, PrometheusRule, VMRule, HTTPRoute, ExternalSecret, CiliumNetworkPolicy)"
  mkdir -p "$SCHEMA_CACHE"
  for i in "${!case_names[@]}"; do
    [ -s "$work/case-$i.yaml" ] || continue
    if out="$(kubeconform -strict -summary -kubernetes-version "$K8S_VERSION" -cache "$SCHEMA_CACHE" \
        -schema-location default -schema-location "$CRD_SCHEMAS" < "$work/case-$i.yaml" 2>&1)"; then
      # -summary prints "Skipped: N"; a skipped CRD would mean the schema was not found and nothing was checked.
      if grep -q 'Skipped: [1-9]' <<<"$out"; then bad "kubeconform: ${case_names[$i]} (resources skipped)" "$out"
      else ok "kubeconform: ${case_names[$i]} ($(grep -o 'Valid: [0-9]*' <<<"$out"))"; fi
    else
      bad "kubeconform: ${case_names[$i]}" "$out"
    fi
  done
fi

section "rendered manifest invariants (hardening, probes, no latest, no secrets, no RBAC)"
for i in "${!case_names[@]}"; do
  [ -s "$work/case-$i.yaml" ] || continue
  if out="$(python3 -I "$here/check_rendered.py" < "$work/case-$i.yaml" 2>&1)"; then ok "invariants: ${case_names[$i]}"
  else bad "invariants: ${case_names[$i]}" "$out"; fi
done
# Reconcile with the application: every key the chart writes into config.yaml must exist in
# config.example.yaml (the schema's own annotated example). Unknown keys are startup errors, so a key
# the chart invents would be a pod that never starts, and no render check can see that.
example="$repo/config.example.yaml"
if [ -f "$example" ]; then
  for i in "${!case_names[@]}"; do
    [ -s "$work/case-$i.yaml" ] || continue
    if out="$(python3 -I - "$work/case-$i.yaml" "$example" <<'PY'
import sys
import yaml

def leaves(node, prefix=""):
    if isinstance(node, dict) and node:
        for key, value in node.items():
            yield from leaves(value, f"{prefix}{key}.")
    else:
        yield prefix.rstrip(".")

known = set(leaves(yaml.safe_load(open(sys.argv[2]))))
# A key the schema defines as a free-form map would show up as a longer path; the example has none.
unknown = []
for doc in yaml.safe_load_all(open(sys.argv[1])):
    if doc and doc["kind"] == "ConfigMap" and "config.yaml" in doc.get("data", {}):
        rendered = yaml.safe_load(doc["data"]["config.yaml"])
        unknown += [k for k in leaves(rendered) if k not in known]
print(", ".join(sorted(set(unknown))))
sys.exit(1 if unknown else 0)
PY
)"; then :; else bad "config keys unknown to config.example.yaml in case: ${case_names[$i]}" "$out"; fail_cfg=1; fi
  done
  [ -z "${fail_cfg:-}" ] && ok "every rendered config key exists in config.example.yaml (all ${#case_names[@]} cases)"
else
  echo "  skip  config.example.yaml not found, config keys not reconciled"
fi
# SRE-03: the accepted cascade risk is written next to the readinessProbe in the shipped manifest.
if grep -q 'Readiness depends on PostgreSQL only' "$work/case-0.yaml"; then ok "readiness comment (SRE-03) is in the rendered Deployment"
else bad "the readinessProbe lacks the PostgreSQL dependency comment (SRE-03)"; fi

# Manifest scanner (OPS-42). kube-linter runs when installed; MANIFEST_SCANNER replaces it with any
# command that reads one rendered manifest file given as its last argument (checkov, kubescape ...).
scanner="${MANIFEST_SCANNER:-}"
if [ -z "$scanner" ] && command -v kube-linter >/dev/null 2>&1; then scanner="kube-linter lint --config $here/kube-linter.yaml"; fi
if [ -n "$scanner" ]; then
  section "manifest scanner: ${scanner%% *}"
  for i in "${!case_names[@]}"; do
    [ -s "$work/case-$i.yaml" ] || continue
    # shellcheck disable=SC2086
    if out="$($scanner "$work/case-$i.yaml" 2>&1)"; then ok "scanner: ${case_names[$i]}"
    else bad "scanner: ${case_names[$i]}" "$out"; fi
  done
else
  section "manifest scanner"
  echo "  skip  no scanner installed (brew install kube-linter, or set MANIFEST_SCANNER)"
fi

# --------------------------------------------------------------------------------------------
# Negative cases: each of these MUST fail to render, with the stated reason. A rule that cannot
# fail is not a rule, so the tests of the rules are part of the suite.
# --------------------------------------------------------------------------------------------
section "negative cases (must fail to render)"
expect_fail() { # expect_fail <name> <regex> <helm args...>
  local name="$1" regex="$2"; shift 2
  local out
  if out="$(render "$@")"; then bad "must fail: $name (rendered successfully)"
  elif grep -Eqi "$regex" <<<"$out"; then ok "must fail: $name"
  else bad "must fail: $name (failed, but not for the expected reason /$regex/)" "$out"; fi
}
base="-f $ci/base-values.yaml"
# shellcheck disable=SC2086
{
expect_fail "image tag latest (schema)"            "image/tag"                  $base --set image.tag=latest
expect_fail "image tag latest without schema"      "latest"                     $base --set image.tag=latest --skip-schema-validation
expect_fail "image tag LATEST without schema"      "latest"                     $base --set image.tag=LATEST --skip-schema-validation
expect_fail "digest required but missing"          "requireDigest"              -f "$chart/values-prod.yaml"
expect_fail "malformed digest"                     "image/digest"               $base --set image.digest=sha256:abc
expect_fail "local storage with 2 replicas"        "incompatible with HA"       $base --set storage.type=local
expect_fail "local RWX without confirmation"       "confirmReadWriteMany"       $base --set storage.type=local --set 'storage.local.accessModes={ReadWriteMany}'
expect_fail "ingress and gateway together"         "mutually exclusive"         $base -f "$ci/ingress-values.yaml" -f "$ci/gateway-values.yaml"
expect_fail "production with one replica"          "at least 2 replicas"        $base --set replicaCount=1
expect_fail "read-write root filesystem"           "readOnlyRootFilesystem"     $base --set securityContext.readOnlyRootFilesystem=false
expect_fail "privilege escalation"                 "allowPrivilegeEscalation"   $base --set securityContext.allowPrivilegeEscalation=true
expect_fail "run as root"                          "runAsNonRoot"               $base --set podSecurityContext.runAsNonRoot=false
expect_fail "aggregated health endpoint"           "aggregated health"          $base --set probes.liveness.httpGet.path=/actuator/health
expect_fail "probe on the Service port"            "must equal the container port" $base --set probes.readiness.httpGet.port=80
expect_fail "initialDelaySeconds with startupProbe" "initialDelaySeconds"       $base --set probes.liveness.initialDelaySeconds=5
expect_fail "credentials in a URL"                 "credentials"                $base --set 'config.observability.otlp_endpoint=http://u:p@h:4317' --skip-schema-validation
expect_fail "DSN in the config"                    "config.database.url"        $base --set 'config.database.url=postgres://u:p@h/db' --skip-schema-validation
expect_fail "secret key in config.extra"           "is a secret"                $base --set config.extra.auth.cookie_key=abc
expect_fail "plain ldap without StartTLS"          "starttls"                   $base --set config.auth.ldap.url=ldap://h:389
expect_fail "metrics without a token"              "Bearer token"               $base --set secrets.entries.metricsToken=false
expect_fail "production without signing key"       "internalJwtKey"             $base --set secrets.entries.internalJwtKey=false
expect_fail "redis lock without its secret entry"  "lockRedisUrl"               $base --set config.lock.backend=redis
expect_fail "missing postgresql network peer"      "postgresql.to is empty"     -f "$chart/values-test.yaml" --set networkPolicy.egress.postgresql.to=null
expect_fail "empty secret name"                    "existingSecret"             $base --set secrets.existingSecret=
expect_fail "missing base_url"                     "base_url"                   $base --set config.server.base_url=
expect_fail "PDB that blocks every drain"          "block every node drain"     $base --set pdb.maxUnavailable=null --set pdb.minAvailable=2
expect_fail "HPA minimum below replicaCount"       "minReplicas must not be below" $base -f "$ci/autoscaling-values.yaml" --set replicaCount=5
}

# A broken or incomplete alert rule file must stop the render instead of producing an empty or
# partial PrometheusRule that would look healthy.
broken="$work/broken-chart"
cp -R "$chart" "$broken"
printf 'groups: [\n' > "$broken/files/rustryx-alerts.yaml"
if helm template r "$broken" -f "$ci/base-values.yaml" -f "$ci/monitoring-prometheus-values.yaml" >/dev/null 2>&1; then
  bad "must fail: invalid alert rule file renders (empty PrometheusRule)"; else ok "must fail: invalid alert rule file"; fi
cp "$chart/files/rustryx-alerts.yaml" "$broken/files/rustryx-alerts.yaml"
python3 -I - "$broken/files/rustryx-alerts.yaml" <<'PY'
import re, sys
p = sys.argv[1]
s = open(p).read()
s = re.sub(r"\n +runbook_url: \S+#rustryxdown\n", "\n", s, count=1)
open(p, "w").write(s)
PY
if helm template r "$broken" -f "$ci/base-values.yaml" -f "$ci/monitoring-prometheus-values.yaml" >/dev/null 2>&1; then
  bad "must fail: alert without runbook_url renders"; else ok "must fail: alert without runbook_url"; fi

# The invariant checker must itself notice violations.
section "invariant checker catches violations"
check_catches() { # check_catches <name> <helm args...>; the case itself must render
  local name="$1" out; shift
  if ! out="$(render "$@")"; then bad "checker case does not render: $name" "$out"
  elif python3 -I "$here/check_rendered.py" <<<"$out" >/dev/null 2>&1; then bad "checker misses: $name"
  else ok "checker catches: $name"; fi
}
# shellcheck disable=SC2086
{
check_catches "literal password in env"   $base --set 'extraEnv[0].name=DB_PASSWORD' --set 'extraEnv[0].value=hunter2'
check_catches "VAULT variable in env"     $base --set 'extraEnv[0].name=VAULT_ADDR' --set 'extraEnv[0].value=https://vault'
check_catches "readiness on /healthz"     $base --set probes.readiness.httpGet.path=/healthz
check_catches "container running as 1000" $base --set securityContext.runAsUser=1000
check_catches "no preStop pause"          $base --set shutdown.preStopSleepSeconds=0
}

# --------------------------------------------------------------------------------------------
section "values hygiene (no secrets, no latest, no dashes)"
files="$(ls "$chart"/values*.yaml "$ci"/*.yaml)"
# A key that names a credential with a literal value of 6+ characters. Names of Secrets and of keys
# inside a secret store are references, not values, and are listed explicitly.
# shellcheck disable=SC2086
hits="$(grep -n -i -E '^[[:space:]]*[A-Za-z_]*(password|passwd|secret|token|apikey|api_key|access_?key|private_?key)[A-Za-z_]*[[:space:]]*:[[:space:]]*["'\'']?[A-Za-z0-9+/=_.-]{6,}' $files \
  | grep -v -E 'existingSecret|bearerTokenSecret|secretStoreRef|secretKey:|secretName|existingConfigMap' || true)"
if [ -z "$hits" ]; then ok "no credential-like literal in values or ci files"; else bad "credential-like literal in values" "$hits"; fi
# shellcheck disable=SC2086
if grep -n -E 'BEGIN [A-Z ]*PRIVATE KEY|AKIA[0-9A-Z]{16}|://[^/@[:space:]]+:[^/@[:space:]]+@' $files "$chart"/templates/*.yaml "$chart"/templates/*.tpl >/dev/null; then
  # shellcheck disable=SC2086
  bad "private key, cloud key or user:password@ URL found" "$(grep -n -E 'BEGIN [A-Z ]*PRIVATE KEY|AKIA[0-9A-Z]{16}|://[^/@[:space:]]+:[^/@[:space:]]+@' $files "$chart"/templates/*.yaml "$chart"/templates/*.tpl)"
else ok "no private key, cloud key or URL credential in values and templates"; fi
# shellcheck disable=SC2086
if grep -n -i -E '(^|[[:space:]])tag:[[:space:]]*["'\'']?latest' $files >/dev/null; then bad "latest tag in a values file"; else ok "no latest tag in values files"; fi
dashes="$(LC_ALL=C grep -rn -e $'\xe2\x80\x93' -e $'\xe2\x80\x94' "$chart" "$mon" "$here/test.sh" "$here/check_rendered.py" \
  "$repo/docs/runbook.md" "$repo/docs/slo.md" "$repo/docs/helm.md" 2>/dev/null || true)"
if [ -z "$dashes" ]; then ok "no em dash or en dash in owned files"; else bad "em or en dash found" "$dashes"; fi
# Probes are values-driven and rendered once: the template must not wrap them in a range, because
# a probe emitted per service port would carry the Service port instead of the containerPort.
if out="$(python3 -I - "$chart/templates/deployment.yaml" <<'PY'
import re, sys
stack: list[str] = []
bad: list[int] = []
for no, line in enumerate(open(sys.argv[1]), 1):
    if re.search(r"(startup|readiness|liveness)Probe:", line) and "range" in stack:
        bad.append(no)
    for tok in re.findall(r"\{\{-?\s*(range|if|with|define|end)\b", line):
        if tok == "end":
            if stack:
                stack.pop()
        else:
            stack.append(tok)
print(bad)
sys.exit(1 if bad else 0)
PY
)"; then ok "probe blocks sit outside any range in deployment.yaml"
else bad "a probe block sits inside a range in deployment.yaml (lines $out)"; fi

# Every top-level key must be explained in docs/helm.md, so a new value cannot ship undocumented.
if out="$(python3 -I - "$chart/values.yaml" "$repo/docs/helm.md" <<'PY'
import re, sys
import yaml
keys = list(yaml.safe_load(open(sys.argv[1])))
doc = open(sys.argv[2]).read()
missing = [k for k in keys if not re.search(r"`" + re.escape(k) + r"[`.\[]", doc)]
print(", ".join(missing))
sys.exit(1 if missing else 0)
PY
)"; then ok "every top-level key of values.yaml is documented in docs/helm.md"
else bad "values keys missing from docs/helm.md: $out"; fi

section "monitoring assets"
if diff -q "$mon/rustryx-alerts.yaml" "$chart/files/rustryx-alerts.yaml" >/dev/null \
   && diff -q "$mon/rustryx-dashboard.json" "$chart/files/rustryx-dashboard.json" >/dev/null; then
  ok "chart files/ match deploy/monitoring sources"
else bad "chart files/ differ from deploy/monitoring: run deploy/monitoring/sync-to-chart.sh"; fi
if out="$(promtool check rules "$mon/rustryx-alerts.yaml" 2>&1)"; then ok "promtool check rules: $(grep -o '[0-9]* rules found' <<<"$out")"; else bad "promtool check rules" "$out"; fi
if out="$(cd "$mon" && promtool test rules tests/rustryx-alerts_test.yaml 2>&1)"; then ok "promtool test rules"; else bad "promtool test rules" "$out"; fi
if out="$(python3 -I "$mon/check_assets.py" 2>&1)"; then ok "$out"; else bad "monitoring asset checks" "$out"; fi
if jq -e . "$mon/rustryx-dashboard.json" >/dev/null 2>&1; then ok "dashboard is valid JSON"; else bad "dashboard JSON is invalid"; fi
python3 -I "$mon/check_assets.py" --emit-promtool "$work/dashboard-queries.yaml" >/dev/null 2>&1 || true
if out="$(promtool check rules "$work/dashboard-queries.yaml" 2>&1)"; then ok "every dashboard query parses ($(grep -o '[0-9]* rules found' <<<"$out"))"; else bad "a dashboard query does not parse" "$out"; fi
size="$(wc -c < "$mon/rustryx-dashboard.json" | tr -d ' ')"
# A ConfigMap is limited to 1 MiB including everything else in it.
if [ "$size" -lt 900000 ]; then ok "dashboard fits a ConfigMap (${size} bytes)"; else bad "dashboard too large for a ConfigMap (${size} bytes)"; fi
# The rendered rules must be what promtool accepts too (the chart rewrites names and labels).
render -f "$ci/base-values.yaml" -f "$ci/monitoring-prometheus-values.yaml" -s templates/rules.yaml > "$work/rules.yaml" 2>&1 || true
if [ -s "$work/rules.yaml" ] && python3 -I - "$work/rules.yaml" "$work/rules-plain.yaml" <<'PY'
import sys, yaml
doc = next(d for d in yaml.safe_load_all(open(sys.argv[1])) if d)
yaml.safe_dump({"groups": doc["spec"]["groups"]}, open(sys.argv[2], "w"))
PY
then
  if out="$(promtool check rules "$work/rules-plain.yaml" 2>&1)"; then ok "promtool check rules on the RENDERED PrometheusRule"; else bad "promtool on rendered rules" "$out"; fi
else bad "could not extract rendered rules"; fi

section "packaging"
if out="$(helm package "$chart" --destination "$work" 2>&1)"; then
  pkg="$(ls "$work"/rustryx-*.tgz)"
  listing="$(tar -tzf "$pkg")"
  grep -q 'rustryx/files/rustryx-dashboard.json' <<<"$listing" && grep -q 'rustryx/files/rustryx-alerts.yaml' <<<"$listing" \
    && ok "package contains the monitoring files" || bad "package misses files/"
  grep -q 'rustryx/ci/\|rustryx/tests/' <<<"$listing" && bad "package contains ci/ or tests/ (.helmignore)" || ok "package excludes ci/ and tests/"
else bad "helm package" "$out"; fi

if [ "${SKIP_UNITTEST:-0}" != 1 ]; then
  section "helm unittest"
  if out="$(helm unittest "$chart" 2>&1)"; then ok "unittest: $(grep -E '^Tests:' <<<"$out")"; else bad "helm unittest" "$out"; fi
  # Template-level defences that the schema normally pre-empts, with schema validation disabled.
  if out="$(helm unittest "$chart" -f 'tests/noschema/*_test.yaml' --skip-schema-validation 2>&1)"; then ok "unittest (schema skipped): $(grep -E '^Tests:' <<<"$out")"; else bad "helm unittest (schema skipped)" "$out"; fi
fi

printf '\n%d checks passed, %d failed\n' "$passes" "${#failures[@]}"
if [ "${#failures[@]}" -gt 0 ]; then
  printf 'FAILED:\n'; printf '  - %s\n' "${failures[@]}"
  exit 1
fi
