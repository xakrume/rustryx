-- Users, roles, privileges, selectors, tokens, group mappings and login counters.

CREATE TABLE users (
    id                 uuid PRIMARY KEY,
    username           text        NOT NULL CHECK (username <> '' AND length(username) <= 256),
    source             text        NOT NULL CHECK (source IN ('local', 'ldap', 'oidc')),
    -- issuer and subject are separate columns and never concatenated: "a|b" + "c" and
    -- "a" + "b|c" must stay different identities.
    issuer             text,
    subject            text,
    email              text,
    display_name       text,
    enabled            boolean     NOT NULL DEFAULT true,
    password_hash      text,
    tokens_valid_after timestamptz,
    created_at         timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT ck_users_oidc_identity CHECK (
        source <> 'oidc' OR (coalesce(issuer, '') <> '' AND coalesce(subject, '') <> '')
    ),
    CONSTRAINT ck_users_local_no_external CHECK (
        source <> 'local' OR (issuer IS NULL AND subject IS NULL)
    ),
    -- Only local accounts authenticate by password.
    CONSTRAINT ck_users_hash_only_local CHECK (source = 'local' OR password_hash IS NULL),
    -- A plaintext password in this column would sit in every backup. Only PHC argon2
    -- strings and bcrypt hashes (imported accounts) are accepted, so a caller that passes
    -- the password instead of its hash fails here, not silently.
    CONSTRAINT ck_users_password_hash_format CHECK (
        password_hash IS NULL OR password_hash ~ '^\$(argon2(id|i|d)|2[aby])\$.{20,}$'
    )
);
-- Case-insensitive and global: "Admin" must not coexist with "admin" across sources, which
-- is the account-takeover path the cross-source username conflict rule exists to close.
CREATE UNIQUE INDEX uq_users_username_ci ON users (lower(username));
-- Partial: accounts without a subject (local) are not constrained against each other.
CREATE UNIQUE INDEX uq_users_external ON users (source, coalesce(issuer, ''), subject)
    WHERE subject IS NOT NULL;

CREATE TABLE content_selectors (
    id          uuid PRIMARY KEY,
    name        text        NOT NULL CHECK (name <> '' AND length(name) <= 128),
    description text        NOT NULL DEFAULT '',
    -- 1 KiB cap on the expression source: bounds the work the selector engine can be made to do.
    expression  text        NOT NULL CHECK (expression <> '' AND octet_length(expression) <= 1024),
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_selectors_name UNIQUE (name)
);

CREATE TABLE privileges (
    id          uuid PRIMARY KEY,
    name        text        NOT NULL CHECK (name <> '' AND length(name) <= 128),
    description text        NOT NULL DEFAULT '',
    builtin     boolean     NOT NULL DEFAULT false,
    grant_kind  text        NOT NULL CHECK (grant_kind IN ('admin', 'repository-view', 'repository-content-selector')),
    grant_format      text  CHECK (grant_format ~ '^[a-z][a-z0-9_]{0,31}$'),
    grant_repository  text  CHECK (grant_repository <> ''),
    -- RESTRICT: deleting a selector that a privilege still uses must fail, not widen
    -- or silently drop that privilege.
    grant_selector_id uuid  REFERENCES content_selectors (id) ON DELETE RESTRICT,
    actions     text[]      NOT NULL CHECK (
        cardinality(actions) > 0 AND actions <@ ARRAY['browse', 'read', 'write', 'delete']::text[]
    ),
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_privileges_name UNIQUE (name),
    CONSTRAINT ck_privileges_grant_shape CHECK (
        (grant_kind = 'admin'
            AND grant_format IS NULL AND grant_repository IS NULL AND grant_selector_id IS NULL)
        OR (grant_kind = 'repository-view'
            AND grant_repository IS NOT NULL AND grant_selector_id IS NULL)
        OR (grant_kind = 'repository-content-selector'
            AND grant_repository IS NOT NULL AND grant_selector_id IS NOT NULL AND grant_format IS NULL)
    )
);
CREATE INDEX ix_privileges_selector ON privileges (grant_selector_id) WHERE grant_selector_id IS NOT NULL;

CREATE TABLE roles (
    id          uuid PRIMARY KEY,
    name        text        NOT NULL CHECK (name <> '' AND length(name) <= 128),
    description text        NOT NULL DEFAULT '',
    builtin     boolean     NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_roles_name UNIQUE (name)
);

CREATE TABLE role_privileges (
    role_id      uuid NOT NULL REFERENCES roles (id) ON DELETE CASCADE,
    privilege_id uuid NOT NULL REFERENCES privileges (id) ON DELETE CASCADE,
    PRIMARY KEY (role_id, privilege_id)
);
CREATE INDEX ix_role_privileges_privilege ON role_privileges (privilege_id);

-- parent_role_id contains child_role_id: members of the parent get the child's privileges.
CREATE TABLE role_roles (
    parent_role_id uuid NOT NULL REFERENCES roles (id) ON DELETE CASCADE,
    child_role_id  uuid NOT NULL REFERENCES roles (id) ON DELETE CASCADE,
    PRIMARY KEY (parent_role_id, child_role_id),
    CONSTRAINT ck_role_roles_not_self CHECK (parent_role_id <> child_role_id)
);
CREATE INDEX ix_role_roles_child ON role_roles (child_role_id);

CREATE FUNCTION role_roles_reject_cycle() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    -- One global lock for every nesting write. Two transactions adding A->B and B->A at
    -- the same time each see only committed rows, so without serialization both pass the
    -- check below and together create a cycle. Nesting edits are rare admin operations, so
    -- the lock costs nothing. It is held to the end of the transaction, and the check runs
    -- after it is granted, on a fresh READ COMMITTED snapshot that includes the winner.
    PERFORM pg_advisory_xact_lock(hashtextextended('rustryx.role_roles', 0));
    -- Adding parent -> child closes a cycle exactly when parent is already reachable from child.
    IF EXISTS (
        WITH RECURSIVE reach(role_id) AS (
            SELECT NEW.child_role_id
            UNION
            SELECT rr.child_role_id FROM role_roles rr JOIN reach r ON rr.parent_role_id = r.role_id
        )
        SELECT 1 FROM reach WHERE role_id = NEW.parent_role_id
    ) THEN
        RAISE EXCEPTION 'role nesting cycle' USING ERRCODE = 'RX001';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER role_roles_no_cycle
    BEFORE INSERT OR UPDATE ON role_roles
    FOR EACH ROW EXECUTE FUNCTION role_roles_reject_cycle();

-- origin: who put the role on the user. IdP synchronisation replaces only rows of its
-- own origin, so a manual grant survives a Keycloak group change.
CREATE TABLE user_roles (
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- CASCADE: deleting a role takes its assignments with it.
    role_id uuid NOT NULL REFERENCES roles (id) ON DELETE CASCADE,
    origin  text NOT NULL CHECK (origin IN ('manual', 'oidc', 'ldap')),
    PRIMARY KEY (user_id, role_id, origin)
);
CREATE INDEX ix_user_roles_role ON user_roles (role_id);

CREATE TABLE user_tokens (
    id             uuid PRIMARY KEY,
    user_id        uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name           text        NOT NULL CHECK (name <> '' AND length(name) <= 256),
    -- Lowercase hex SHA-256 of the whole token. The token has 256 bits of entropy, so a slow
    -- hash would only add latency; the UNIQUE constraint is also the lookup index.
    token_hash     text        NOT NULL CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    display_prefix text        NOT NULL,
    scopes         text[]      NOT NULL DEFAULT '{}',
    expires_at     timestamptz,
    revoked_at     timestamptz,
    last_used_at   timestamptz,
    created_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_user_tokens_hash UNIQUE (token_hash)
);
CREATE INDEX ix_user_tokens_user ON user_tokens (user_id);

CREATE TABLE idp_group_mappings (
    id             uuid PRIMARY KEY,
    provider       text        NOT NULL CHECK (provider IN ('oidc', 'ldap')),
    -- Exact and case sensitive (Keycloak full path or LDAP DN); never normalised here.
    external_group text        NOT NULL CHECK (external_group <> '' AND length(external_group) <= 1024),
    -- RESTRICT: deleting a role that a mapping points to must be an explicit two-step action.
    role_id        uuid        NOT NULL REFERENCES roles (id) ON DELETE RESTRICT,
    priority       integer     NOT NULL DEFAULT 0,
    created_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_idp_mapping_group UNIQUE (provider, external_group)
);
CREATE INDEX ix_idp_group_mappings_role ON idp_group_mappings (role_id);

-- ip = '' is the per-account counter, a real address is the (account, ip) counter. The
-- sentinel keeps both in one primary key (a NULL cannot be part of it) and makes the
-- increment a single atomic INSERT .. ON CONFLICT DO UPDATE.
CREATE TABLE login_attempts (
    account         text        NOT NULL CHECK (account <> ''),
    ip              text        NOT NULL DEFAULT '',
    failures        integer     NOT NULL CHECK (failures >= 0),
    last_failure_at timestamptz NOT NULL,
    PRIMARY KEY (account, ip)
);
CREATE INDEX ix_login_attempts_last_failure ON login_attempts (last_failure_at);
