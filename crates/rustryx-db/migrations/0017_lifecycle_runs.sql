-- Lifecycle run history (wave2-design section 4, cleanup and retention jobs).
-- cleanup_policy_id has no foreign key: cleanup_policies belongs to another stream.
-- Items keep a deleted component on purpose (no foreign key on component_id).
-- Runs and items are kept 180 days after finish.

CREATE TABLE lifecycle_runs (
    id uuid PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('cleanup', 'retention', 'label_cleanup')),
    cleanup_policy_id uuid,
    retention_policy_id uuid REFERENCES retention_policies (id) ON DELETE SET NULL,
    repository_id uuid REFERENCES repositories (id) ON DELETE SET NULL,
    dry_run boolean NOT NULL,
    policy_version bigint NOT NULL,
    task_run_id uuid,
    status text NOT NULL,
    counts jsonb NOT NULL DEFAULT '{}'::jsonb,
    truncated boolean NOT NULL DEFAULT false,
    requested_by uuid,
    confirmed_mass_delete boolean NOT NULL DEFAULT false,
    started_at timestamptz,
    finished_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX ix_lifecycle_runs_policy ON lifecycle_runs (cleanup_policy_id, created_at DESC);
CREATE INDEX ix_lifecycle_runs_retention ON lifecycle_runs (retention_policy_id, created_at DESC);

CREATE TABLE lifecycle_run_items (
    run_id uuid NOT NULL REFERENCES lifecycle_runs (id) ON DELETE CASCADE,
    seq bigint NOT NULL,
    repository text NOT NULL,
    component_id uuid NOT NULL,
    coordinate text NOT NULL,
    tags text[] NOT NULL DEFAULT '{}',
    size_bytes bigint NOT NULL,
    decision text NOT NULL,
    reason text NOT NULL,
    PRIMARY KEY (run_id, seq)
);
