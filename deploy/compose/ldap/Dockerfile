# OpenLDAP fixture image for local development and integration tests.
# Built here (not pulled) because the usual community images are unmaintained or
# have no memberOf overlay, and because the base can be pinned by digest.
# Test-only: it generates a throwaway CA, ships plaintext test passwords from
# seed.ldif and must never be exposed beyond the developer machine.
FROM alpine:3.24.3@sha256:be2656061b536c1883c4034790e21d0a09c5aaaf51968b89368a3c0371ea2c9a

# The alpine tag above is fixed, so the package index it resolves against does
# not move; pinning each package would only break when that index is refreshed.
# hadolint ignore=DL3018
RUN apk add --no-cache \
      openldap \
      openldap-back-mdb \
      openldap-overlay-memberof \
      openldap-clients \
      openssl \
    && mkdir -p /run/openldap /var/lib/openldap/openldap-data \
    && chown -R ldap:ldap /run/openldap /var/lib/openldap

COPY slapd.conf.tmpl /etc/openldap/slapd.conf.tmpl
COPY entrypoint.sh gen-certs.sh /usr/local/bin/
# The execute bit does not survive every way of building this image (a build
# context assembled from single files by a test harness drops it), so set it here.
RUN chmod 0755 /usr/local/bin/entrypoint.sh /usr/local/bin/gen-certs.sh

# 389 ldap (StartTLS available), 636 ldaps. Plain ldap is reachable inside the
# compose network only for StartTLS tests; Rustryx itself refuses ldap:// without
# StartTLS (README 8.4).
EXPOSE 389 636
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
