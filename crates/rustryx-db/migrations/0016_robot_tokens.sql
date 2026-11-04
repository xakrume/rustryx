-- Robot tokens (ADR-W2-2). A table of their own: an old pod ignores it, and
-- deleting the creator must not delete the robot (created_by is SET NULL).
-- Deleting a project is RESTRICT while robots still point at it.
-- expiry_notice_at is the mark of token_expiry_notice, not a field of the domain row.

CREATE TABLE robot_tokens (
    id               uuid PRIMARY KEY,
    name             text        NOT NULL,
    short_name       text        NOT NULL,
    project_id       uuid        REFERENCES projects (id) ON DELETE RESTRICT,
    scopes           text[]      NOT NULL,
    token_hash       text        NOT NULL,
    display_prefix   text        NOT NULL,
    expires_at       timestamptz,
    disabled_at      timestamptz,
    revoked_at       timestamptz,
    last_used_at     timestamptz,
    rotated_at       timestamptz,
    expiry_notice_at timestamptz,
    created_by       uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    version          bigint      NOT NULL DEFAULT 1,
    CONSTRAINT ck_robot_tokens_name CHECK (name ~ '^[A-Za-z0-9][A-Za-z0-9+._-]{0,127}$'),
    CONSTRAINT ck_robot_tokens_short_name CHECK (
        short_name ~ '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$'),
    CONSTRAINT ck_robot_tokens_scopes CHECK (cardinality(scopes) BETWEEN 1 AND 64),
    CONSTRAINT ck_robot_tokens_hash CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT uq_robot_tokens_hash UNIQUE (token_hash)
);

CREATE UNIQUE INDEX uq_robot_tokens_name ON robot_tokens (lower(name));
CREATE INDEX ix_robot_tokens_project ON robot_tokens (project_id) WHERE project_id IS NOT NULL;
CREATE INDEX ix_robot_tokens_list ON robot_tokens (name);
CREATE INDEX ix_robot_tokens_expiry ON robot_tokens (expires_at)
    WHERE expires_at IS NOT NULL AND revoked_at IS NULL;
