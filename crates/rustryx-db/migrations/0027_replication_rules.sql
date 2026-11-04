-- Replication rules. Executions and tasks are not created yet, so a rule can still be
-- edited and deleted. A registry that a rule names cannot be deleted.

CREATE TABLE replication_rules (
    id uuid PRIMARY KEY,
    name text NOT NULL CHECK (name <> '' AND length(name) <= 128),
    direction text NOT NULL CHECK (direction IN ('push', 'pull')),
    registry_id uuid NOT NULL REFERENCES registries (id) ON DELETE RESTRICT,
    repository_id uuid REFERENCES repositories (id) ON DELETE CASCADE,
    project_id uuid REFERENCES projects (id) ON DELETE CASCADE,
    remote_namespace text NOT NULL DEFAULT '' CHECK (length(remote_namespace) <= 255),
    flatten_levels smallint NOT NULL DEFAULT 0 CHECK (flatten_levels BETWEEN 0 AND 3),
    override_existing boolean NOT NULL DEFAULT false,
    filter jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(filter) = 'object'),
    trigger text NOT NULL CHECK (trigger IN ('manual', 'scheduled', 'event')),
    on_push boolean NOT NULL DEFAULT false,
    on_delete boolean NOT NULL DEFAULT false,
    schedule_task_id uuid,
    copy_referrers boolean NOT NULL DEFAULT true,
    replicate_deletion boolean NOT NULL DEFAULT false,
    bandwidth_limit_kbps integer CHECK (bandwidth_limit_kbps IS NULL OR bandwidth_limit_kbps > 0),
    event_settle_secs integer NOT NULL DEFAULT 30 CHECK (event_settle_secs BETWEEN 0 AND 600),
    enabled boolean NOT NULL DEFAULT true,
    version bigint NOT NULL DEFAULT 1,
    created_by uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_replication_rules_name UNIQUE (name),
    CONSTRAINT ck_replication_scope CHECK ((repository_id IS NULL) <> (project_id IS NULL))
);
