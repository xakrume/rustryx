-- Cleanup policies (wave2-design section 4). Repositories already store the ids
-- in cleanup_policy_ids (0002). lifecycle_runs.cleanup_policy_id arrived in 0017
-- without a foreign key because this table did not exist yet.

CREATE TABLE cleanup_policies (
    id uuid PRIMARY KEY,
    name text NOT NULL CHECK (name <> '' AND length(name) <= 128),
    description text NOT NULL DEFAULT '' CHECK (length(description) <= 500),
    format text CHECK (format IS NULL OR format ~ '^[a-z][a-z0-9_]{0,31}$'),
    criteria jsonb NOT NULL CHECK (jsonb_typeof(criteria) = 'object'),
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_cleanup_policies_name UNIQUE (name)
);
CREATE INDEX ix_repositories_cleanup_policies ON repositories USING gin (cleanup_policy_ids);

ALTER TABLE lifecycle_runs
    ADD CONSTRAINT lifecycle_runs_cleanup_policy_fk
    FOREIGN KEY (cleanup_policy_id) REFERENCES cleanup_policies (id) ON DELETE SET NULL;
