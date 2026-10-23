#!/bin/sh
# Start slapd with the fixture data. The seed is loaded ONLINE (through the
# server) because the memberOf overlay only reacts to LDAP operations, not to
# slapadd. During seeding slapd listens on the unix socket only, so a client
# never sees a half-loaded directory; the real listeners start afterwards.
set -eu

: "${LDAP_ADMIN_PASSWORD:?LDAP_ADMIN_PASSWORD must be set (see deploy/compose/.env.example)}"
ROOT_DN="cn=admin,dc=rustryx,dc=test"
SEED="${LDAP_SEED_FILE:-/seed/seed.ldif}"
CONF=/etc/openldap/slapd.conf
# Explicit socket path inside a directory owned by the unprivileged slapd user:
# the compiled-in default (/var/run/ldapi) does not exist in this image, and
# slapd opens the listener after it has dropped root.
SOCK_URL="ldapi://%2Frun%2Fopenldap%2Fldapi"

# Certificates are generated only when none were provided: integration tests copy
# their own CA and server certificate into /certs before the start, and those must
# not be replaced. Renewal of the compose certificates is the job of the `certs`
# service, not of this entrypoint.
if [ ! -s /certs/ca.crt ] || [ ! -s /certs/server.crt ] || [ ! -s /certs/server.key ]; then
  /usr/local/bin/gen-certs.sh
fi
# slapd reads the key as the unprivileged user `ldap`; a copied-in key is often
# 0600 root. These are throwaway test keys, so make them readable.
chmod a+r /certs/ca.crt /certs/server.crt /certs/server.key 2>/dev/null || true

rootpw=$(slappasswd -h '{SSHA}' -s "$LDAP_ADMIN_PASSWORD")
sed "s|@ROOTPW@|$rootpw|" /etc/openldap/slapd.conf.tmpl >"$CONF"
chmod 0640 "$CONF"
chown root:ldap "$CONF"
slaptest -u -f "$CONF" >/dev/null

if [ ! -s /var/lib/openldap/openldap-data/data.mdb ]; then
  slapd -d 0 -u ldap -g ldap -f "$CONF" -h "$SOCK_URL" &
  pid=$!
  i=0
  until ldapwhoami -x -H "$SOCK_URL" -D "$ROOT_DN" -w "$LDAP_ADMIN_PASSWORD" >/dev/null 2>&1; do
    i=$((i + 1))
    [ "$i" -le 50 ] || { echo "slapd did not come up on the unix socket" >&2; exit 1; }
    sleep 0.2
  done
  ldapadd -x -H "$SOCK_URL" -D "$ROOT_DN" -w "$LDAP_ADMIN_PASSWORD" >/dev/null <<LDIF
dn: dc=rustryx,dc=test
objectClass: top
objectClass: dcObject
objectClass: organization
o: Rustryx test fixture
dc: rustryx
LDIF
  # -a: records without changetype are additions; seed.ldif also has modify records.
  ldapmodify -a -x -H "$SOCK_URL" -D "$ROOT_DN" -w "$LDAP_ADMIN_PASSWORD" -f "$SEED" >/dev/null
  kill "$pid"
  wait "$pid" || true
fi

exec slapd -d 256 -u ldap -g ldap -f "$CONF" -h "ldap:/// ldaps:/// $SOCK_URL"
