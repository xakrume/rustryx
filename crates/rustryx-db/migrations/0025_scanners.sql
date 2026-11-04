-- Registered scanners. Credentials inside auth are sealed envelopes.
-- One default among scanners that report vulnerabilities. Scan policies are not created yet,
-- so a delete is not blocked by them.

CREATE TABLE scanners (
    id uuid PRIMARY KEY,
    name text NOT NULL CHECK (name <> '' AND length(name) <= 128),
    kind text NOT NULL CHECK (kind IN ('trivy', 'osv', 'http')),
    endpoint text CHECK (endpoint IS NULL OR endpoint ~ '^https://'),
    auth jsonb NOT NULL DEFAULT '{"type":"none"}'::jsonb,
    ca_certificate_ids uuid[] NOT NULL DEFAULT '{}',
    capabilities text[] NOT NULL CHECK (cardinality(capabilities) >= 1),
    formats text[] NOT NULL DEFAULT '{}',
    is_default boolean NOT NULL DEFAULT false,
    enabled boolean NOT NULL DEFAULT true,
    last_probe jsonb,
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_scanners_name UNIQUE (name),
    CONSTRAINT ck_scanners_http_endpoint CHECK ((kind = 'http') = (endpoint IS NOT NULL))
);
CREATE UNIQUE INDEX uq_scanners_default_vuln ON scanners ((true))
    WHERE is_default AND 'vulnerability' = ANY (capabilities);
