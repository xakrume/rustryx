-- E-mail subscriptions and the messages they produce. A suppressed row is the record of a
-- mail that was not sent because the same address, kind and subject went out within 10 minutes.

CREATE TABLE notification_subscriptions (
    id uuid PRIMARY KEY,
    project_id uuid REFERENCES projects (id) ON DELETE CASCADE,
    repository_id uuid REFERENCES repositories (id) ON DELETE CASCADE,
    system boolean NOT NULL DEFAULT false,
    events text[] NOT NULL CHECK (cardinality(events) BETWEEN 1 AND 32),
    recipients jsonb NOT NULL CHECK (
        jsonb_typeof(recipients) = 'array' AND jsonb_array_length(recipients) BETWEEN 1 AND 20
    ),
    enabled boolean NOT NULL DEFAULT true,
    version bigint NOT NULL DEFAULT 1,
    created_by uuid,
    CONSTRAINT ck_notify_scope CHECK (num_nonnulls(project_id, repository_id) + (system::int) = 1)
);

CREATE TABLE email_deliveries (
    id uuid PRIMARY KEY,
    subscription_id uuid REFERENCES notification_subscriptions (id) ON DELETE SET NULL,
    event_id uuid NOT NULL,
    event_kind text NOT NULL,
    subject_key text NOT NULL,
    to_address text NOT NULL CHECK (length(to_address) <= 254),
    status text NOT NULL CHECK (status IN (
        'pending', 'sending', 'sent', 'failed', 'dead', 'suppressed'
    )),
    attempts smallint NOT NULL DEFAULT 0,
    next_attempt_at timestamptz,
    lease_owner text,
    lease_expires_at timestamptz,
    last_error text CHECK (last_error IS NULL OR length(last_error) <= 64),
    created_at timestamptz NOT NULL DEFAULT now(),
    sent_at timestamptz
);
CREATE INDEX ix_email_claim ON email_deliveries (next_attempt_at)
    WHERE status IN ('pending', 'sending');
CREATE INDEX ix_email_dedupe ON email_deliveries (to_address, event_kind, subject_key, created_at DESC);
