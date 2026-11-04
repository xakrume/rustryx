-- Webhooks. The secret and the optional auth header are sealed envelopes. A scope is exactly
-- one of system, project or repository. At most 20 rows per scope.

CREATE TABLE webhooks (
    id uuid PRIMARY KEY,
    project_id uuid REFERENCES projects (id) ON DELETE CASCADE,
    repository_id uuid REFERENCES repositories (id) ON DELETE CASCADE,
    system boolean NOT NULL DEFAULT false,
    name text NOT NULL CHECK (name <> '' AND length(name) <= 128),
    kind text NOT NULL CHECK (kind IN ('http', 'cloudevents')),
    url text NOT NULL CHECK (url ~ '^https?://[^@]+$' AND length(url) <= 2048),
    secret text NOT NULL,
    auth_header text,
    events text[] NOT NULL CHECK (cardinality(events) BETWEEN 1 AND 32),
    enabled boolean NOT NULL DEFAULT true,
    suspended_at timestamptz,
    consecutive_failures integer NOT NULL DEFAULT 0,
    retry_max smallint NOT NULL DEFAULT 5 CHECK (retry_max BETWEEN 1 AND 10),
    timeout_secs smallint NOT NULL DEFAULT 10 CHECK (timeout_secs BETWEEN 1 AND 30),
    version bigint NOT NULL DEFAULT 1,
    created_by uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT ck_webhooks_scope CHECK (num_nonnulls(project_id, repository_id) + (system::int) = 1)
);
CREATE INDEX ix_webhooks_project ON webhooks (project_id) WHERE project_id IS NOT NULL AND enabled;
CREATE INDEX ix_webhooks_repository ON webhooks (repository_id) WHERE repository_id IS NOT NULL AND enabled;
CREATE INDEX ix_webhooks_system ON webhooks (system) WHERE system AND enabled;

CREATE FUNCTION webhooks_cap() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    n integer;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(
        coalesce(NEW.project_id::text, '') || ':' || coalesce(NEW.repository_id::text, '') || ':' || NEW.system::text,
        0));
    SELECT count(*) INTO n FROM webhooks
     WHERE system = NEW.system
       AND project_id IS NOT DISTINCT FROM NEW.project_id
       AND repository_id IS NOT DISTINCT FROM NEW.repository_id;
    IF n >= 20 THEN
        RAISE EXCEPTION 'a scope has at most 20 webhooks' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER webhooks_cap BEFORE INSERT ON webhooks
    FOR EACH ROW EXECUTE FUNCTION webhooks_cap();
