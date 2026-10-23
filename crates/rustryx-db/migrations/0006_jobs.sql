-- Durable work queues with leases.
--
-- A worker claims one row with FOR UPDATE SKIP LOCKED, so concurrent workers on any node
-- never wait on each other and never claim the same row. The claim sets an expiry; a
-- worker that dies stops heartbeating, the lease expires and another node picks the row up
-- (state 'running' with an expired lease counts as claimable).

CREATE TABLE job_leases (
    id               uuid PRIMARY KEY,
    queue            text        NOT NULL CHECK (queue <> '' AND length(queue) <= 128),
    -- At most one queued or running row per (queue, dedupe_key): enqueueing the same
    -- logical job twice is a no-op, not two runs.
    dedupe_key       text,
    payload          jsonb       NOT NULL DEFAULT '{}'::jsonb,
    state            text        NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'running', 'done', 'failed')),
    attempts         integer     NOT NULL DEFAULT 0,
    available_at     timestamptz NOT NULL DEFAULT now(),
    lease_owner      text,
    lease_expires_at timestamptz,
    heartbeat_at     timestamptz,
    last_error       text,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ix_job_leases_claim ON job_leases (queue, available_at) WHERE state IN ('queued', 'running');
CREATE UNIQUE INDEX uq_job_leases_dedupe ON job_leases (queue, dedupe_key)
    WHERE dedupe_key IS NOT NULL AND state IN ('queued', 'running');
