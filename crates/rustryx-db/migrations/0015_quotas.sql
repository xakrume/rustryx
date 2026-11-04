-- Project quotas and the usage counters of projects and repositories (HB-30).
-- A repository limit stays in repositories.quota_bytes. quota_usage.version is the
-- optimistic lock for both scopes.

CREATE TABLE quotas (
    project_id   uuid PRIMARY KEY REFERENCES projects (id) ON DELETE CASCADE,
    limit_bytes  bigint CHECK (limit_bytes IS NULL OR limit_bytes >= 0),
    warn_percent smallint NOT NULL DEFAULT 80 CHECK (warn_percent BETWEEN 1 AND 100),
    version      bigint NOT NULL DEFAULT 1,
    updated_by   uuid,
    updated_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE quota_usage (
    scope_kind   text NOT NULL CHECK (scope_kind IN ('project', 'repository')),
    scope_id     uuid NOT NULL,
    used_bytes   bigint NOT NULL DEFAULT 0 CHECK (used_bytes >= 0),
    object_count bigint NOT NULL DEFAULT 0 CHECK (object_count >= 0),
    limit_bytes  bigint,
    warn_percent smallint NOT NULL DEFAULT 80 CHECK (warn_percent BETWEEN 1 AND 100),
    version      bigint NOT NULL DEFAULT 1,
    recounted_at timestamptz,
    PRIMARY KEY (scope_kind, scope_id)
);
