-- One delivery of one event to one webhook. No foreign key: deleting a webhook must not
-- scan this table. A replay of the same pair inserts nothing.

CREATE TABLE webhook_deliveries (
    id uuid PRIMARY KEY,
    webhook_id uuid NOT NULL,
    event_id uuid NOT NULL,
    event_kind text NOT NULL,
    attempt smallint NOT NULL DEFAULT 0,
    status text NOT NULL CHECK (status IN ('pending', 'delivering', 'succeeded', 'failed', 'dead')),
    next_attempt_at timestamptz,
    lease_owner text,
    lease_expires_at timestamptz,
    last_status_code smallint,
    last_latency_ms integer,
    last_error text CHECK (last_error IS NULL OR length(last_error) <= 64),
    response_excerpt text CHECK (response_excerpt IS NULL OR length(response_excerpt) <= 1024),
    created_at timestamptz NOT NULL DEFAULT now(),
    delivered_at timestamptz,
    CONSTRAINT uq_webhook_delivery_event UNIQUE (webhook_id, event_id)
);
CREATE INDEX ix_webhook_deliveries_claim ON webhook_deliveries (next_attempt_at)
    WHERE status IN ('pending', 'delivering');
CREATE INDEX ix_webhook_deliveries_webhook ON webhook_deliveries (webhook_id, created_at DESC);
