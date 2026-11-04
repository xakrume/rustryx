-- Named remote registries. Credentials inside auth are sealed envelopes.

CREATE TABLE registries (
    id uuid PRIMARY KEY,
    name text NOT NULL CHECK (name <> '' AND length(name) <= 128),
    provider text NOT NULL CHECK (provider IN ('rustryx', 'oci')),
    url text NOT NULL CHECK (url ~ '^https?://[^/@?#]+$'),
    auth jsonb NOT NULL DEFAULT '{"type":"none"}'::jsonb,
    ca_certificate_ids uuid[] NOT NULL DEFAULT '{}',
    last_test jsonb,
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_registries_name UNIQUE (name)
);
