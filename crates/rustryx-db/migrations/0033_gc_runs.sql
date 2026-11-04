-- Garbage collection history. The sweep itself is a later job; this table is the
-- record a stop request and a dry-run sample are read from.
-- Samples keep the object key after the blob row is gone (no foreign key).
-- A deleted blob store leaves the run (ON DELETE SET NULL) so the report stays.

CREATE TABLE gc_runs (
    id uuid PRIMARY KEY,
    trigger text NOT NULL CHECK (trigger IN ('schedule', 'manual', 'event', 'startup')),
    status text NOT NULL CHECK (status IN (
        'queued', 'running', 'succeeded', 'failed', 'cancelled', 'skipped', 'abandoned'
    )),
    dry_run boolean NOT NULL,
    delete_untagged boolean NOT NULL,
    min_age_secs bigint NOT NULL CHECK (min_age_secs >= 0),
    workers smallint NOT NULL CHECK (workers BETWEEN 1 AND 8),
    blob_store_id uuid REFERENCES blob_stores (id) ON DELETE SET NULL,
    examined bigint NOT NULL DEFAULT 0 CHECK (examined >= 0),
    candidates bigint NOT NULL DEFAULT 0 CHECK (candidates >= 0),
    deleted bigint NOT NULL DEFAULT 0 CHECK (deleted >= 0),
    failed bigint NOT NULL DEFAULT 0 CHECK (failed >= 0),
    bytes_candidates bigint NOT NULL DEFAULT 0 CHECK (bytes_candidates >= 0),
    bytes_deleted bigint NOT NULL DEFAULT 0 CHECK (bytes_deleted >= 0),
    untagged_deleted bigint NOT NULL DEFAULT 0 CHECK (untagged_deleted >= 0),
    per_store jsonb NOT NULL DEFAULT '{}'::jsonb,
    stop_requested boolean NOT NULL DEFAULT false,
    task_run_id uuid,
    requested_by uuid,
    started_at timestamptz,
    finished_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX ix_gc_runs_created ON gc_runs (created_at DESC, id DESC);
CREATE INDEX ix_gc_runs_finished ON gc_runs (finished_at) WHERE finished_at IS NOT NULL;

CREATE TABLE gc_samples (
    run_id uuid NOT NULL REFERENCES gc_runs (id) ON DELETE CASCADE,
    seq integer NOT NULL CHECK (seq BETWEEN 1 AND 1000),
    blob_store text NOT NULL,
    object_key text NOT NULL,
    size_bytes bigint NOT NULL CHECK (size_bytes >= 0),
    orphaned_at timestamptz NOT NULL,
    PRIMARY KEY (run_id, seq)
);
