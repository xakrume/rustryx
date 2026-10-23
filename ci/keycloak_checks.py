#!/usr/bin/env python3
"""Checks of the shared Keycloak realm fixture against a RUNNING Keycloak.

Called by ci/verify-fixtures.sh. Standard library only. The realm forbids the
password grant, so tokens are obtained the way the UI does it: authorization
code with PKCE S256, driving the login form with a cookie jar.

Usage: keycloak_checks.py --base https://localhost:8443 --ca deploy/compose/certs/ca.crt \
           --admin-user admin --admin-password ... --ui-secret ... --cli-secret ...
Exit code 0 when every check passed, 1 otherwise (each failure is printed).
"""

import argparse
import base64
import hashlib
import html
import http.cookiejar
import json
import re
import secrets
import ssl
import sys
import urllib.error
import urllib.parse
import urllib.request

REALM = "rustryx"
UI_REDIRECT = "http://localhost:8081/api/v1/auth/oidc/callback"
USERS = {  # username -> expected groups claim (the passwords are public test values)
    "alice": ["/rustryx/admins"],
    "bob": ["/rustryx/readers"],
    "carol": ["/rustryx/team-payments/publishers"],
    "dave": [],
    "erin": ["/rustryx/team-x/admins"],
}


class NoRedirect(urllib.request.HTTPRedirectHandler):
    """Surface 3xx responses instead of following them (the target is not running)."""

    def redirect_request(self, *args, **kwargs):  # noqa: D401 - urllib hook
        return None


class Checker:
    def __init__(self, base: str, ca: str) -> None:
        self.base = base.rstrip("/")
        self.ctx = ssl.create_default_context(cafile=ca)
        self.failures: list = []

    # ---- reporting -------------------------------------------------------
    def check(self, name: str, ok: bool, detail: str = "") -> None:
        print(f"{'ok  ' if ok else 'FAIL'} {name}" + (f" ({detail})" if detail and not ok else ""))
        if not ok:
            self.failures.append(name)

    # ---- HTTP ------------------------------------------------------------
    def opener(self, jar=None):
        handlers = [
            urllib.request.HTTPSHandler(context=self.ctx),
            NoRedirect(),
        ]
        if jar is not None:
            handlers.append(urllib.request.HTTPCookieProcessor(jar))
        return urllib.request.build_opener(*handlers)

    def request(self, opener, url, data=None, headers=None, method=None):
        body = urllib.parse.urlencode(data).encode() if isinstance(data, dict) else data
        req = urllib.request.Request(url, data=body, headers=headers or {}, method=method)
        try:
            with opener.open(req, timeout=30) as resp:
                return resp.status, dict(resp.headers), resp.read()
        except urllib.error.HTTPError as err:
            return err.code, dict(err.headers), err.read()

    def realm_url(self, path: str, realm: str = REALM) -> str:
        return f"{self.base}/realms/{realm}/{path}"

    # ---- tokens ----------------------------------------------------------
    @staticmethod
    def claims(jwt: str) -> dict:
        payload = jwt.split(".")[1]
        return json.loads(base64.urlsafe_b64decode(payload + "=" * (-len(payload) % 4)))

    def client_credentials(self, client_id: str, secret: str, scope: str = "") -> tuple:
        data = {"grant_type": "client_credentials", "client_id": client_id, "client_secret": secret}
        if scope:
            data["scope"] = scope
        status, _, raw = self.request(self.opener(), self.realm_url("protocol/openid-connect/token"), data)
        return status, json.loads(raw or b"{}")

    def password_grant(self, client_id: str, secret: str) -> tuple:
        data = {"grant_type": "password", "client_id": client_id, "client_secret": secret,
                "username": "alice", "password": "alice-test-only"}
        status, _, raw = self.request(self.opener(), self.realm_url("protocol/openid-connect/token"), data)
        return status, json.loads(raw or b"{}")

    def auth_request(self, jar, scope="openid", pkce=True) -> tuple:
        verifier = secrets.token_urlsafe(48)
        params = {"client_id": "rustryx-ui", "redirect_uri": UI_REDIRECT, "response_type": "code",
                  "scope": scope, "state": secrets.token_urlsafe(8), "nonce": secrets.token_urlsafe(8)}
        if pkce:
            digest = hashlib.sha256(verifier.encode()).digest()
            params["code_challenge"] = base64.urlsafe_b64encode(digest).rstrip(b"=").decode()
            params["code_challenge_method"] = "S256"
        url = self.realm_url("protocol/openid-connect/auth") + "?" + urllib.parse.urlencode(params)
        status, headers, body = self.request(self.opener(jar), url)
        return status, headers, body.decode("utf-8", "replace"), verifier

    def login_code(self, username: str, password: str, scope="openid") -> tuple:
        """Returns (redirect Location after login, PKCE verifier) or (error text, None)."""
        jar = http.cookiejar.CookieJar()
        status, headers, page, verifier = self.auth_request(jar, scope)
        match = re.search(r'<form[^>]*id="kc-form-login"[^>]*action="([^"]+)"', page) or \
            re.search(r'action="([^"]*login-actions/authenticate[^"]*)"', page)
        if not match:
            return headers.get("Location", f"no login form, HTTP {status}"), None
        action = html.unescape(match.group(1))
        status, headers, _ = self.request(
            self.opener(jar), action, {"username": username, "password": password, "credentialId": ""})
        return headers.get("Location", f"HTTP {status}"), verifier

    def user_token(self, username: str, password: str) -> dict:
        location, verifier = self.login_code(username, password)
        query = urllib.parse.parse_qs(urllib.parse.urlparse(location).query)
        if verifier is None or "code" not in query:
            raise RuntimeError(f"login of {username} gave no code: {location}")
        status, _, raw = self.request(self.opener(), self.realm_url("protocol/openid-connect/token"), {
            "grant_type": "authorization_code", "code": query["code"][0], "redirect_uri": UI_REDIRECT,
            "client_id": "rustryx-ui", "client_secret": self.ui_secret, "code_verifier": verifier})
        if status != 200:
            raise RuntimeError(f"token exchange for {username}: HTTP {status} {raw[:200]!r}")
        return json.loads(raw)

    # ---- admin API -------------------------------------------------------
    def admin(self, path: str, method="GET", data=None):
        headers = {"Authorization": f"Bearer {self.admin_token}"}
        body = None
        if data is not None:
            body, headers["Content-Type"] = json.dumps(data).encode(), "application/json"
        status, _, raw = self.request(self.opener(), f"{self.base}/admin/realms/{REALM}{path}", body, headers, method)
        return status, (json.loads(raw) if raw else None)

    def login_admin(self, user: str, password: str) -> None:
        status, _, raw = self.request(self.opener(), self.realm_url("protocol/openid-connect/token", "master"), {
            "grant_type": "password", "client_id": "admin-cli", "username": user, "password": password})
        if status != 200:
            raise RuntimeError(f"master admin login failed: HTTP {status}")
        self.admin_token = json.loads(raw)["access_token"]

    # ---- the checks ------------------------------------------------------
    def run(self, args) -> None:
        self.ui_secret = args.ui_secret
        self.login_admin(args.admin_user, args.admin_password)
        self.check_users()
        self.check_membership_change()
        self.check_cli_client(args.cli_secret)
        self.check_forbidden_flows(args.ui_secret, args.cli_secret)
        self.check_realm_settings()

    def check_users(self) -> None:
        for name, expected in USERS.items():
            token = self.user_token(name, f"{name}-test-only")
            claims = self.claims(token["access_token"])
            groups = claims.get("groups", [])
            aud = claims.get("aud", [])
            aud = [aud] if isinstance(aud, str) else aud
            self.check(f"{name}: groups claim is {expected}", sorted(groups) == sorted(expected), str(groups))
            self.check(f"{name}: aud contains rustryx-ui", "rustryx-ui" in aud, str(aud))
            self.check(f"{name}: azp is rustryx-ui", claims.get("azp") == "rustryx-ui", str(claims.get("azp")))
            if name == "alice":
                issuer = f"{self.base}/realms/{REALM}"
                self.check("alice: groups carries /rustryx/admins", "/rustryx/admins" in groups)
                self.check("alice: iss equals the public issuer", claims.get("iss") == issuer, str(claims.get("iss")))
                self.check("alice: access token lives 300 s", claims["exp"] - claims["iat"] == 300,
                           str(claims["exp"] - claims["iat"]))
                self.check("alice: no refresh token of type Offline",
                           self.claims(token.get("refresh_token", "x.e30.x")).get("typ", "Refresh") != "Offline"
                           if token.get("refresh_token") else True)
            if name == "erin":
                self.check("erin: look-alike group does not carry /rustryx/admins", "/rustryx/admins" not in groups)

    def check_membership_change(self) -> None:
        status, users = self.admin("/users?username=bob&exact=true")
        status_g, group = self.admin("/group-by-path/rustryx/readers")
        if status != 200 or status_g != 200 or not users:
            self.check("bob membership change: resolve ids", False, f"{status}/{status_g}")
            return
        uid, gid = users[0]["id"], group["id"]
        before = self.claims(self.user_token("bob", "bob-test-only")["access_token"]).get("groups", [])
        try:
            status, _ = self.admin(f"/users/{uid}/groups/{gid}", "DELETE")
            after = self.claims(self.user_token("bob", "bob-test-only")["access_token"]).get("groups", [])
        finally:
            restore, _ = self.admin(f"/users/{uid}/groups/{gid}", "PUT")
        self.check("bob: before removal groups has /rustryx/readers", "/rustryx/readers" in before, str(before))
        self.check("bob: removal answered 204", status == 204, str(status))
        self.check("bob: next token no longer has /rustryx/readers", "/rustryx/readers" not in after, str(after))
        self.check("bob: membership restored for later runs", restore == 204, str(restore))

    def check_cli_client(self, secret: str) -> None:
        status, body = self.client_credentials("rustryx-cli", secret)
        if status != 200:
            self.check("rustryx-cli: client credentials token", False, f"HTTP {status}")
            return
        claims = self.claims(body["access_token"])
        aud = claims.get("aud", [])
        aud = [aud] if isinstance(aud, str) else aud
        self.check("rustryx-cli: groups is /rustryx/ci/demo", claims.get("groups") == ["/rustryx/ci/demo"],
                   str(claims.get("groups")))
        self.check("rustryx-cli: aud has rustryx-cli and rustryx-ui", {"rustryx-cli", "rustryx-ui"} <= set(aud), str(aud))
        self.check("rustryx-cli: client credentials gives no refresh token", "refresh_token" not in body)

    def check_forbidden_flows(self, ui_secret: str, cli_secret: str) -> None:
        for client, secret in (("rustryx-ui", ui_secret), ("rustryx-cli", cli_secret)):
            status, body = self.password_grant(client, secret)
            self.check(f"{client}: password grant is refused", status in (400, 401) and "access_token" not in body,
                       f"HTTP {status}")
        status, body = self.client_credentials("rustryx-cli", cli_secret, scope="offline_access")
        scope = body.get("scope", "")
        self.check("rustryx-cli: offline_access is refused or ignored",
                   status != 200 or ("offline_access" not in scope.split() and "refresh_token" not in body),
                   f"HTTP {status} scope={scope!r}")
        location, verifier = self.login_code("alice", "alice-test-only", scope="openid offline_access")
        query = urllib.parse.parse_qs(urllib.parse.urlparse(location).query)
        refused = verifier is None or "error" in query
        if not refused:
            status, _, raw = self.request(self.opener(), self.realm_url("protocol/openid-connect/token"), {
                "grant_type": "authorization_code", "code": query["code"][0], "redirect_uri": UI_REDIRECT,
                "client_id": "rustryx-ui", "client_secret": self.ui_secret, "code_verifier": verifier})
            refresh = json.loads(raw).get("refresh_token") if status == 200 else None
            refused = refresh is None or self.claims(refresh).get("typ") != "Offline"
        self.check("rustryx-ui: scope offline_access never yields an offline token", refused, location[:120])
        jar = http.cookiejar.CookieJar()
        status, headers, page, _ = self.auth_request(jar, pkce=False)
        self.check("rustryx-ui: authorization request without PKCE is refused",
                   "kc-form-login" not in page, f"HTTP {status}")

    def check_realm_settings(self) -> None:
        status, realm = self.admin("")
        self.check("realm: read through the admin API", status == 200, str(status))
        if status != 200:
            return
        for key, want in (("accessTokenLifespan", 300), ("ssoSessionIdleTimeout", 1800),
                          ("ssoSessionMaxLifespan", 36000), ("offlineSessionMaxLifespanEnabled", False)):
            self.check(f"realm: {key} is {want}", realm.get(key) == want, str(realm.get(key)))
        # The default role keeps the standard offline_access composite: a realm
        # import cannot change it (the import ignores composites of the default
        # role). Offline tokens are therefore blocked per client, by removing the
        # offline_access scope below, and by the product rejecting such tokens.
        for client_id in ("rustryx-ui", "rustryx-cli"):
            _, found = self.admin(f"/clients?clientId={client_id}")
            client = found[0] if found else {}
            self.check(f"{client_id}: confidential", client.get("publicClient") is False)
            self.check(f"{client_id}: implicit and password grants off",
                       client.get("implicitFlowEnabled") is False and client.get("directAccessGrantsEnabled") is False)
            _, optional = self.admin(f"/clients/{client.get('id')}/optional-client-scopes")
            self.check(f"{client_id}: offline_access not an optional scope",
                       "offline_access" not in {s["name"] for s in optional or []})
            if client_id == "rustryx-ui":
                self.check("rustryx-ui: PKCE S256 enforced",
                           client.get("attributes", {}).get("pkce.code.challenge.method") == "S256")
                self.check("rustryx-ui: redirect URIs have no wildcard",
                           not any("*" in uri for uri in client.get("redirectUris", [])))
            else:
                self.check("rustryx-cli: service account and device grant on",
                           client.get("serviceAccountsEnabled") is True
                           and client.get("attributes", {}).get("oauth2.device.authorization.grant.enabled") == "true")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--base", required=True)
    parser.add_argument("--ca", required=True)
    parser.add_argument("--admin-user", required=True)
    parser.add_argument("--admin-password", required=True)
    parser.add_argument("--ui-secret", required=True)
    parser.add_argument("--cli-secret", required=True)
    args = parser.parse_args()
    checker = Checker(args.base, args.ca)
    try:
        checker.run(args)
    except (RuntimeError, urllib.error.URLError, KeyError, ValueError) as exc:
        print(f"FAIL aborted: {exc}")
        return 1
    print(f"{len(checker.failures)} failed")
    return 1 if checker.failures else 0


if __name__ == "__main__":
    sys.exit(main())
