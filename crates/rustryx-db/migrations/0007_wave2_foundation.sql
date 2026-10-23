-- Wave 2 foundation (design wave2-design.md section 1.5, Core revision 2).
--
-- Only what more than one stream references, so that the streams start in parallel with their
-- foreign keys in place. EXPAND ONLY: nothing here is read by a binary of the previous release
-- (schema 6), and the one trigger that reaches an existing table (`repositories`) is safe for an
-- N-1 writer. `schema_contract.min_binary_schema` is therefore NOT touched: an N-1 binary keeps
-- serving against this schema (`SchemaStatus::OlderCompatible`).
--
-- `routing_rules` and `cleanup_policies` are not here on purpose (their owners add them in their
-- own migration range), and neither is anything stream specific.

-- Projects: the container. Full table; W2-B owns later additions.
CREATE TABLE projects (
    id          uuid PRIMARY KEY,
    -- The rule of `ProjectName`: lowercase letters and digits with single '.', '_' or '-' between.
    name        text        NOT NULL CHECK (name ~ '^[a-z0-9]+([._-][a-z0-9]+)*$' AND length(name) BETWEEN 2 AND 64),
    description text        NOT NULL DEFAULT '' CHECK (length(description) <= 4000),
    visibility  text        NOT NULL DEFAULT 'private' CHECK (visibility IN ('private', 'public')),
    created_by  uuid        REFERENCES users (id) ON DELETE SET NULL,
    version     bigint      NOT NULL DEFAULT 1,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_projects_name UNIQUE (name)
);

-- RESTRICT: a project that still has repositories cannot disappear (no cascade into content).
ALTER TABLE repositories ADD COLUMN project_id uuid REFERENCES projects (id) ON DELETE RESTRICT;
CREATE INDEX ix_repositories_project ON repositories (project_id) WHERE project_id IS NOT NULL;

-- Invariant: `allow_anonymous` of a repository in a project follows the project visibility. An
-- N-1 binary that updates a repository row and writes its old `allow_anonymous` value is
-- overruled by this trigger, so the invariant holds for every writer.
CREATE FUNCTION repositories_project_sync() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.project_id IS NOT NULL THEN
        NEW.allow_anonymous := (SELECT visibility = 'public' FROM projects WHERE id = NEW.project_id);
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER repositories_project_sync BEFORE INSERT OR UPDATE OF project_id, allow_anonymous ON repositories
    FOR EACH ROW EXECUTE FUNCTION repositories_project_sync();

-- The other direction: a visibility change rewrites its repositories (which fires the trigger
-- above on each of them) and tells the other nodes. `pg_notify` inside the transaction is
-- delivered after commit only, so no node reacts to a change that rolled back.
CREATE FUNCTION projects_visibility_sync() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE repositories SET allow_anonymous = (NEW.visibility = 'public') WHERE project_id = NEW.id;
    PERFORM pg_notify('rustryx_rbac', 'projects');
    RETURN NULL;
END $$;
CREATE TRIGGER projects_visibility_sync AFTER UPDATE OF visibility ON projects
    FOR EACH ROW EXECUTE FUNCTION projects_visibility_sync();

-- Uploader, pull time, audit scope.
ALTER TABLE components ADD COLUMN created_by uuid REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE components ADD COLUMN last_downloaded_at timestamptz;
ALTER TABLE assets     ADD COLUMN created_by uuid REFERENCES users (id) ON DELETE SET NULL;
-- Plain text like `kind`: a new actor kind needs no migration. An N-1 writer that does not know
-- the column is stored as a user, which is what it always meant.
ALTER TABLE audit_events ADD COLUMN actor_kind text NOT NULL DEFAULT 'user';
-- No foreign key: audit rows outlive the project they describe.
ALTER TABLE audit_events ADD COLUMN project_id uuid;
CREATE INDEX ix_audit_events_project ON audit_events (project_id, event_time DESC) WHERE project_id IS NOT NULL;

-- Settings revision and author (design section 8). The revision comes from one sequence, so
-- max(revision) is a monotonic high water mark that nodes compare ("which settings have you
-- applied").
CREATE SEQUENCE system_config_revision_seq;
ALTER TABLE system_config ADD COLUMN revision   bigint NOT NULL DEFAULT nextval('system_config_revision_seq');
ALTER TABLE system_config ADD COLUMN updated_by uuid;
CREATE FUNCTION system_config_bump() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN NEW.revision := nextval('system_config_revision_seq'); NEW.updated_at := now(); RETURN NEW; END $$;
-- A trigger and not application code: the N-1 `PgSystemConfig::set` updates the row without
-- knowing the revision, and a write that did not bump it would hide a change from the nodes.
CREATE TRIGGER system_config_bump BEFORE UPDATE ON system_config
    FOR EACH ROW EXECUTE FUNCTION system_config_bump();

-- Revoked internal JWTs (core gap 21): a revocation list in memory is per node and breaks
-- revocation in a cluster.
CREATE TABLE revoked_tokens (
    tid        uuid PRIMARY KEY,
    expires_at timestamptz NOT NULL
);
CREATE INDEX ix_revoked_tokens_expiry ON revoked_tokens (expires_at);

-- Node registry and feature levels (design section 2.4).
CREATE TABLE node_heartbeats (
    node_id           text PRIMARY KEY CHECK (node_id <> '' AND length(node_id) <= 200),
    version           text        NOT NULL,
    feature_level     integer     NOT NULL CHECK (feature_level >= 1),
    started_at        timestamptz NOT NULL,
    seen_at           timestamptz NOT NULL,
    settings_revision bigint      NOT NULL DEFAULT 0,
    inflight_writes   integer     NOT NULL DEFAULT 0,
    details           jsonb       NOT NULL DEFAULT '{}'::jsonb
);

-- Event outbox and consumer cursors (design section 2.2).
CREATE SEQUENCE event_outbox_seq;
CREATE TABLE event_outbox (
    seq           bigint      NOT NULL DEFAULT nextval('event_outbox_seq'),
    -- The xid8 of the inserting transaction: commit order can differ from sequence order, so a
    -- consumer reads (tx_id, seq) below the snapshot horizon only.
    tx_id         xid8        NOT NULL DEFAULT pg_current_xact_id(),
    id            uuid        NOT NULL,
    occurred_at   timestamptz NOT NULL DEFAULT now(),
    kind          text        NOT NULL,
    project_id    uuid,
    repository_id uuid,
    actor_id      uuid,
    actor_name    text        NOT NULL,
    actor_kind    text        NOT NULL,
    request_id    text,
    data          jsonb       NOT NULL,
    PRIMARY KEY (occurred_at, seq)
) PARTITION BY RANGE (occurred_at);
CREATE INDEX ix_event_outbox_relay ON event_outbox (tx_id, seq);
-- The partition key has to be part of every unique constraint of a partitioned table.
CREATE UNIQUE INDEX uq_event_outbox_id ON event_outbox (id, occurred_at);

CREATE TABLE outbox_cursors (
    consumer   text PRIMARY KEY,
    last_tx_id xid8   NOT NULL DEFAULT '0',
    last_seq   bigint NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE FUNCTION outbox_partition_name(p_month date) RETURNS text
LANGUAGE sql IMMUTABLE AS $$
    SELECT 'event_outbox_y' || to_char(p_month, 'YYYY') || 'm' || to_char(p_month, 'MM')
$$;

-- Written like the audit functions of 0005: UTC month boundaries, generated names only, one
-- advisory transaction lock so concurrent callers do not deadlock on the parent's catalog entry.
CREATE FUNCTION ensure_outbox_partitions(
    p_now timestamptz DEFAULT now(),
    p_lookahead_months integer DEFAULT 2
) RETURNS integer LANGUAGE plpgsql AS $$
DECLARE
    v_first   date := date_trunc('month', p_now AT TIME ZONE 'UTC')::date;
    v_month   date;
    v_name    text;
    v_created integer := 0;
BEGIN
    IF p_lookahead_months < 0 OR p_lookahead_months > 24 THEN
        RAISE EXCEPTION 'lookahead must be between 0 and 24 months' USING ERRCODE = '22023';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended('rustryx.outbox_partitions', 0));
    FOR i IN 0..p_lookahead_months LOOP
        v_month := (v_first + make_interval(months => i))::date;
        v_name := outbox_partition_name(v_month);
        IF to_regclass(quote_ident(v_name)) IS NULL THEN
            EXECUTE format(
                'CREATE TABLE %I PARTITION OF event_outbox FOR VALUES FROM (%L) TO (%L)',
                v_name,
                (v_month::timestamp AT TIME ZONE 'UTC'),
                ((v_month + interval '1 month')::date::timestamp AT TIME ZONE 'UTC')
            );
            v_created := v_created + 1;
        END IF;
    END LOOP;
    RETURN v_created;
END
$$;

-- Drops partitions whose whole month is older than p_retention_days AND that hold no event a
-- consumer has not handled yet ("never drop past the slowest cursor"). A cursor is a position
-- `(tx_id, seq)`, so "unhandled" is `(tx_id, seq) > cursor` for some consumer; the check runs
-- in the function and not in the caller so that no caller can forget it. A consumer that has
-- no cursor row has handled nothing but is not waited for: a new consumer starts at the
-- current position by design. Only partitions named by outbox_partition_name() are
-- considered, so a partition attached by hand is never dropped.
CREATE FUNCTION drop_old_outbox_partitions(
    p_now timestamptz DEFAULT now(),
    p_retention_days integer DEFAULT 30
) RETURNS integer LANGUAGE plpgsql AS $$
DECLARE
    v_rec       record;
    v_match     text[];
    v_upper     timestamptz;
    v_unhandled boolean;
    v_dropped   integer := 0;
BEGIN
    IF p_retention_days < 1 THEN
        RAISE EXCEPTION 'retention must be at least one day' USING ERRCODE = '22023';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended('rustryx.outbox_partitions', 0));
    FOR v_rec IN
        SELECT c.relname::text AS relname
        FROM pg_inherits i
        JOIN pg_class c ON c.oid = i.inhrelid
        WHERE i.inhparent = 'event_outbox'::regclass
    LOOP
        v_match := regexp_match(v_rec.relname, '^event_outbox_y([0-9]{4})m([0-9]{2})$');
        CONTINUE WHEN v_match IS NULL;
        v_upper := ((make_date(v_match[1]::integer, v_match[2]::integer, 1) + interval '1 month')::date)::timestamp
                   AT TIME ZONE 'UTC';
        CONTINUE WHEN v_upper > p_now - make_interval(days => p_retention_days);
        EXECUTE format(
            'SELECT EXISTS (SELECT 1 FROM %I e JOIN outbox_cursors c ON (e.tx_id, e.seq) > (c.last_tx_id, c.last_seq))',
            v_rec.relname
        ) INTO v_unhandled;
        CONTINUE WHEN v_unhandled;
        EXECUTE format('DROP TABLE %I', v_rec.relname);
        v_dropped := v_dropped + 1;
    END LOOP;
    RETURN v_dropped;
END
$$;

-- The current month and the lookahead exist from this migration on.
SELECT ensure_outbox_partitions(now(), 2);

-- Task framework (design section 2.3).
CREATE TABLE task_definitions (
    id          uuid PRIMARY KEY,
    kind        text        NOT NULL CHECK (kind ~ '^[a-z][a-z0-9_]{0,63}$'),
    name        text        NOT NULL CHECK (name <> '' AND length(name) <= 128),
    params      jsonb       NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(params) = 'object'),
    schedule    jsonb       NOT NULL CHECK (jsonb_typeof(schedule) = 'object'),
    enabled     boolean     NOT NULL DEFAULT true,
    system      boolean     NOT NULL DEFAULT false,
    notify      jsonb       NOT NULL DEFAULT '{"on_failure": false, "on_success": false}'::jsonb,
    next_run_at timestamptz,
    last_run_id uuid,
    version     bigint      NOT NULL DEFAULT 1,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_task_definitions_name UNIQUE (name)
);
CREATE INDEX ix_task_definitions_due ON task_definitions (next_run_at) WHERE enabled AND next_run_at IS NOT NULL;

CREATE TABLE task_runs (
    id                uuid PRIMARY KEY,
    task_id           uuid        REFERENCES task_definitions (id) ON DELETE SET NULL,
    kind              text        NOT NULL,
    scope_key         text        NOT NULL DEFAULT '',
    -- The wire names of `TaskRunStatus` and `TaskTrigger`.
    status            text        NOT NULL CHECK (status IN ('queued','running','succeeded','failed','cancelled','skipped','abandoned')),
    trigger           text        NOT NULL CHECK (trigger IN ('schedule','manual','event','startup')),
    dry_run           boolean     NOT NULL DEFAULT false,
    params            jsonb       NOT NULL DEFAULT '{}'::jsonb,
    node_id           text,
    lease_expires_at  timestamptz,
    heartbeat_at      timestamptz,
    stop_requested_at timestamptz,
    queued_at         timestamptz NOT NULL DEFAULT now(),
    started_at        timestamptz,
    finished_at       timestamptz,
    summary           jsonb       NOT NULL DEFAULT '{}'::jsonb,
    error             text        CHECK (error IS NULL OR length(error) <= 500),
    attempts          integer     NOT NULL DEFAULT 0
);
-- One queued or running run per (kind, scope): the database side of "singleton task".
CREATE UNIQUE INDEX uq_task_runs_active ON task_runs (kind, scope_key) WHERE status IN ('queued', 'running');
CREATE INDEX ix_task_runs_task ON task_runs (task_id, queued_at DESC);
CREATE INDEX ix_task_runs_claim ON task_runs (queued_at) WHERE status = 'queued';

CREATE TABLE task_run_log (
    run_id  uuid        NOT NULL REFERENCES task_runs (id) ON DELETE CASCADE,
    seq     integer     NOT NULL,
    at      timestamptz NOT NULL DEFAULT now(),
    level   smallint    NOT NULL,
    message text        NOT NULL CHECK (length(message) <= 1024),
    PRIMARY KEY (run_id, seq)
);
