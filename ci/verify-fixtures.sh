#!/usr/bin/env bash
# Boots Keycloak and OpenLDAP from deploy/compose and proves that the SHARED
# fixtures (keycloak/realm-rustryx.json, ldap/seed.ldif) behave as the product
# specification needs (README 8.6, last paragraph):
#   Keycloak: alice's access token carries groups=/rustryx/admins and aud with
#             rustryx-ui; removing bob from a group changes his next token;
#             no password grant, no offline tokens, PKCE required.
#   OpenLDAP: a bind with the right password succeeds, an EMPTY password is
#             refused, anonymous bind is refused, escaping and memberOf work.
# The exit code is non-zero when any check failed. The stack is stopped and its
# volumes removed afterwards unless KEEP_STACK=1.
#
# Usage: ci/verify-fixtures.sh
# Host ports used: ${KEYCLOAK_HTTPS_PORT:-8443}, 1389 and 1636. Needs docker
# compose and python3.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
compose_dir="$root/deploy/compose"
env_file="$compose_dir/.env.example"
failures=0
last_out=""

compose() { docker compose -f "$compose_dir/docker-compose.yml" "$@"; }
env_value() { sed -n "s/^$1=//p" "$env_file" | head -n 1; }
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1${2:+ ($2)}"; failures=$((failures + 1)); }

# run: capture the combined output of a command and keep its exit status.
run() { last_out=$("$@" 2>&1); }
check_ok() { # name command...: passes when the command succeeds
  local name=$1
  shift
  if run "$@"; then pass "$name"; else fail "$name" "$last_out"; fi
}
check_refused() { # name command...: passes when the command FAILS
  local name=$1
  shift
  if run "$@"; then fail "$name" "accepted: $last_out"; else pass "$name"; fi
}
check_output() { # name regex command...: passes when the output matches
  local name=$1 regex=$2
  shift 2
  run "$@"
  if grep -Eqi -- "$regex" <<<"$last_out"; then pass "$name"; else fail "$name" "$last_out"; fi
}
check_entries() { # name expected-count command...: counts dn: lines
  local name=$1 expected=$2
  shift 2
  run "$@"
  if [ "$(grep -c '^dn:' <<<"$last_out")" = "$expected" ]; then pass "$name"; else fail "$name" "$last_out"; fi
}

cleanup() {
  if [ "${KEEP_STACK:-0}" = 1 ]; then
    echo "KEEP_STACK=1: stack left running; stop it with: docker compose -f $compose_dir/docker-compose.yml down -v"
  else
    compose down -v --remove-orphans >/dev/null 2>&1
  fi
}
trap cleanup EXIT

# The realm file and .env.example both hold the client secrets. Two places, so
# the script fails when they drift instead of letting a test discover it later.
realm_secret() {
  python3 -I - "$compose_dir/keycloak/realm-rustryx.json" "$1" <<'PY'
import json, sys
realm = json.load(open(sys.argv[1]))
print(next(c["secret"] for c in realm["clients"] if c["clientId"] == sys.argv[2]))
PY
}
ui_secret=$(env_value RUSTRYX_UI_CLIENT_SECRET)
cli_secret=$(env_value RUSTRYX_CLI_CLIENT_SECRET)
if [ "$(realm_secret rustryx-ui)" = "$ui_secret" ]; then pass "realm secret of rustryx-ui equals .env.example"; else fail "realm secret of rustryx-ui differs from RUSTRYX_UI_CLIENT_SECRET in .env.example"; fi
if [ "$(realm_secret rustryx-cli)" = "$cli_secret" ]; then pass "realm secret of rustryx-cli equals .env.example"; else fail "realm secret of rustryx-cli differs from RUSTRYX_CLI_CLIENT_SECRET in .env.example"; fi

echo "--- starting keycloak and ldap"
if ! compose up -d --build --wait keycloak ldap; then
  compose ps -a
  compose logs --tail 60 keycloak ldap certs
  fail "stack did not become healthy"
  exit 1
fi

echo "--- keycloak"
https_port=${KEYCLOAK_HTTPS_PORT:-$(env_value KEYCLOAK_HTTPS_PORT)}
python3 -I "$root/ci/keycloak_checks.py" \
  --base "https://localhost:${https_port}" \
  --ca "$compose_dir/certs/ca.crt" \
  --admin-user "$(env_value KC_BOOTSTRAP_ADMIN_USERNAME)" \
  --admin-password "$(env_value KC_BOOTSTRAP_ADMIN_PASSWORD)" \
  --ui-secret "$ui_secret" --cli-secret "$cli_secret" || failures=$((failures + 1))

echo "--- ldap"
base="dc=rustryx,dc=test"
svc_dn="uid=svc-rustryx,ou=services,$base"
svc_pw=$(env_value LDAP_SERVICE_PASSWORD)
alice_dn="uid=alice,ou=people,$base"
special_dn='uid=star*(paren)\5Cback\,comma\+plus,ou=people,dc=rustryx,dc=test'

# The clients run inside the container, over the real server certificate trusted
# through the generated CA, with hostname verification on. The URL uses the
# service name `ldap`, not `localhost`: libldap canonicalizes `localhost` through
# a reverse lookup of ::1 (ip6-localhost), which is not in the certificate, so
# `ldaps://localhost` fails the hostname check inside the container although the
# certificate does list `localhost` (rustls clients on the host are not affected).
ldap_url=ldaps://ldap
lc() { compose exec -T -e LDAPTLS_CACERT=/certs/ca.crt ldap "$@"; }
bind() { lc ldapwhoami -x -H "$ldap_url" -D "$1" -w "$2"; }
search() { # base filter attributes...
  lc ldapsearch -LLL -x -H "$ldap_url" -D "$svc_dn" -w "$svc_pw" -b "$1" "$2" "${@:3}"
}

check_ok "alice binds with the right password" bind "$alice_dn" "alice-test-only"
check_refused "bind with an EMPTY password is refused" bind "$alice_dn" ""
check_refused "bind with a wrong password is refused" bind "$alice_dn" "wrong-password"
check_refused "anonymous bind is refused" lc ldapwhoami -x -H "$ldap_url"
check_ok "special-character user binds with its password" bind "$special_dn" "special-test-only"

check_output "memberOf of alice lists cn=rustryx-admins" "^memberOf: cn=rustryx-admins,ou=groups,$base" \
  search "ou=people,$base" "(uid=alice)" memberOf
check_entries "RFC 4515 escaped filter finds exactly the special user" 1 \
  search "ou=people,$base" '(uid=star\2a\28paren\29\5cback,comma+plus)' dn
check_entries "an unescaped * is a wildcard, which is why the product must escape" 1 \
  search "ou=people,$base" '(uid=star*)' dn
check_output "nested chain: chain-l1 is a member of chain-l2" "^memberOf: cn=chain-l2," \
  search "ou=groups,$base" '(cn=chain-l1)' memberOf
check_output "nested chain: chain-l3 is a member of chain-l4" "^memberOf: cn=chain-l4," \
  search "ou=groups,$base" '(cn=chain-l3)' memberOf
check_output "cycle: cycle-a is a member of cycle-b" "^memberOf: cn=cycle-b," \
  search "ou=groups,$base" '(cn=cycle-a)' memberOf
check_output "cycle: cycle-b is a member of cycle-a" "^memberOf: cn=cycle-a," \
  search "ou=groups,$base" '(cn=cycle-b)' memberOf
check_output "group filter with the special user's escaped DN finds readers" "^dn: cn=readers," \
  search "ou=groups,$base" '(member=uid=star\2a\28paren\29\5c5Cback\5c,comma\5c+plus,ou=people,dc=rustryx,dc=test)' dn

run lc ldapsearch -LLL -x -H "$ldap_url" -D "$alice_dn" -w alice-test-only \
  -b "uid=bob,ou=people,$base" -s base userPassword
if grep -qi '^userPassword' <<<"$last_out"; then fail "alice can read bob's userPassword" "$last_out"; else pass "a user cannot read another user's userPassword"; fi

echo "--- result"
if [ "$failures" -eq 0 ]; then
  echo "verify-fixtures: all checks passed"
else
  echo "verify-fixtures: $failures check group(s) failed"
fi
[ "$failures" -eq 0 ]
