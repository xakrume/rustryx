-- One execution of a replication rule, and the tasks that do the copying.
-- One queued or running execution per rule. Finished rows stay until purge.

CREATE TABLE replication_executions (
    id uuid PRIMARY KEY,
    rule_id uuid NOT NULL REFERENCES replication_rules (id) ON DELETE CASCADE,
    trigger text NOT NULL CHECK (trigger IN ('schedule', 'manual', 'event', 'startup')),
    status text NOT NULL CHECK (status IN (
        'queued', 'running', 'succeeded', 'partially_failed', 'failed', 'stopped'
    )),
    counts jsonb NOT NULL DEFAULT '{}'::jsonb,
    stop_requested_at timestamptz,
    requested_by uuid,
    started_at timestamptz,
    finished_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX uq_replication_active ON replication_executions (rule_id)
    WHERE status IN ('queued', 'running');
CREATE INDEX ix_replication_exec_rule ON replication_executions (rule_id, created_at DESC);

CREATE TABLE replication_tasks (
    id uuid PRIMARY KEY,
    execution_id uuid NOT NULL REFERENCES replication_executions (id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('copy', 'copy_referrers', 'delete')),
    repository text NOT NULL,
    image text NOT NULL,
    reference text NOT NULL,
    status text NOT NULL CHECK (status IN (
        'pending', 'running', 'succeeded', 'failed', 'skipped', 'stopped'
    )),
    attempts smallint NOT NULL DEFAULT 0,
    bytes_transferred bigint NOT NULL DEFAULT 0,
    error text CHECK (error IS NULL OR length(error) <= 64),
    available_at timestamptz NOT NULL DEFAULT now(),
    lease_owner text,
    lease_expires_at timestamptz,
    started_at timestamptz,
    finished_at timestamptz
);
CREATE INDEX ix_replication_tasks_claim ON replication_tasks (available_at)
    WHERE status = 'pending';
CREATE INDEX ix_replication_tasks_exec ON replication_tasks (execution_id, status);

CREATE TABLE replication_task_log (
    task_id uuid NOT NULL REFERENCES replication_tasks (id) ON DELETE CASCADE,
    seq integer NOT NULL,
    at timestamptz NOT NULL DEFAULT now(),
    level smallint NOT NULL,
    message text NOT NULL CHECK (length(message) <= 1024),
    PRIMARY KEY (task_id, seq)
);
