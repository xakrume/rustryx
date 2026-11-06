#!/bin/sh
# Throwaway TLS material for the local stack: one CA and one server certificate
# valid for every service name used in compose. Test-only. The CA private key is
# deleted right after signing, so nobody can mint more certificates from the
# files left in the bind mount; to rotate, delete the directory content.
set -eu

dir="${CERT_DIR:-/certs}"
mkdir -p "$dir"
cd "$dir"

if [ -s ca.crt ] && [ -s server.crt ] && [ -s server.key ] \
   && openssl x509 -checkend 2592000 -noout -in server.crt >/dev/null 2>&1; then
  echo "gen-certs: existing certificates are valid for more than 30 days"
  exit 0
fi

openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out ca.key 2>/dev/null
openssl req -x509 -new -key ca.key -sha256 -days 825 \
  -subj "/CN=Rustryx TEST CA (not for production)" \
  -addext "basicConstraints=critical,CA:TRUE" \
  -addext "keyUsage=critical,keyCertSign,cRLSign" \
  -out ca.crt

# PKCS#8 on purpose: the Quarkus (Keycloak) PEM reader and OpenSSL both read it.
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out server.key 2>/dev/null
openssl req -new -key server.key -subj "/CN=localhost" -out server.csr
cat >server.ext <<'EXT'
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = serverAuth
subjectAltName = DNS:localhost,DNS:ldap,DNS:keycloak,DNS:seaweedfs,DNS:rustryx,DNS:rustryx.localhost,IP:127.0.0.1,IP:::1
EXT
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
  -days 825 -sha256 -extfile server.ext -out server.crt 2>/dev/null

rm -f ca.key ca.srl server.csr server.ext
# World-readable on purpose: Keycloak (uid 1000), slapd (ldap) and the host test
# runner all read the same files. Safe only because they are worthless test keys.
chmod 0644 ca.crt server.crt server.key
echo "gen-certs: wrote ca.crt, server.crt, server.key to $dir"
