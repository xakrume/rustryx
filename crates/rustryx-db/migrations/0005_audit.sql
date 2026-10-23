-- Audit log: append-only, partitioned by month.

CREATE TABLE audit_events (
    id          uuid        NOT NULL,
    event_time  timestamptz NOT NULL,
    kind        text        NOT NULL,
    actor_id    uuid,
    actor_name  text        NOT NULL,
    client_ip   text,
    request_id  text,
    action      text        NOT NULL,
    target      text,
    outcome     text        NOT NULL,
    detail      jsonb       NOT NULL DEFAULT '{}'::jsonb,
    -- The partition key has to be part of every unique constraint of a partitioned table.
    PRIMARY KEY (id, event_time)
) PARTITION BY RANGE (event_time);

CREATE INDEX ix_audit_events_time ON audit_events (event_time DESC);
CREATE INDEX ix_audit_events_actor ON audit_events (actor_id, event_time DESC) WHERE actor_id IS NOT NULL;
CREATE INDEX ix_audit_events_kind ON audit_events (kind, event_time DESC);

-- Append-only at the row level. The primary control is the grant (the application role
-- gets INSERT and SELECT only, see docs/db.md); this trigger also stops the table owner
-- from rewriting history by accident. Partition maintenance is unaffected: DROP TABLE of
-- a partition does not fire row triggers.
CREATE FUNCTION audit_events_reject_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'audit_events is append-only' USING ERRCODE = 'RX003';
END
$$;
CREATE TRIGGER audit_events_append_only BEFORE UPDATE OR DELETE ON audit_events
    FOR EACH ROW EXECUTE FUNCTION audit_events_reject_change();
CREATE TRIGGER audit_events_no_truncate BEFORE TRUNCATE ON audit_events
    FOR EACH STATEMENT EXECUTE FUNCTION audit_events_reject_change();

CREATE FUNCTION audit_partition_name(p_month date) RETURNS text
LANGUAGE sql IMMUTABLE AS $$
    SELECT 'audit_events_y' || to_char(p_month, 'YYYY') || 'm' || to_char(p_month, 'MM')
$$;

-- Creates the partition of the month containing p_now and of the next p_lookahead_months
-- months (UTC month boundaries). Idempotent. Returns how many partitions were created.
CREATE FUNCTION ensure_audit_partitions(
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
    -- CREATE TABLE .. PARTITION OF takes locks on the parent; two nodes running this at
    -- once could deadlock or fail on the catalog entry. One advisory lock per transaction
    -- makes the loop single-file and the IF NOT EXISTS check below reliable.
    PERFORM pg_advisory_xact_lock(hashtextextended('rustryx.audit_partitions', 0));
    FOR i IN 0..p_lookahead_months LOOP
        v_month := (v_first + make_interval(months => i))::date;
        v_name := audit_partition_name(v_month);
        IF to_regclass(quote_ident(v_name)) IS NULL THEN
            EXECUTE format(
                'CREATE TABLE %I PARTITION OF audit_events FOR VALUES FROM (%L) TO (%L)',
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

-- Drops partitions whose whole month is older than p_retention_days. Only partitions named
-- by audit_partition_name() are considered, so a partition someone attached by hand
-- (an archive, a legal hold) is never dropped by this function.
CREATE FUNCTION drop_expired_audit_partitions(
    p_now timestamptz DEFAULT now(),
    p_retention_days integer DEFAULT 90
) RETURNS integer LANGUAGE plpgsql AS $$
DECLARE
    v_rec     record;
    v_match   text[];
    v_upper   timestamptz;
    v_dropped integer := 0;
BEGIN
    IF p_retention_days < 1 THEN
        RAISE EXCEPTION 'retention must be at least one day' USING ERRCODE = '22023';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended('rustryx.audit_partitions', 0));
    FOR v_rec IN
        SELECT c.relname::text AS relname
        FROM pg_inherits i
        JOIN pg_class c ON c.oid = i.inhrelid
        WHERE i.inhparent = 'audit_events'::regclass
    LOOP
        v_match := regexp_match(v_rec.relname, '^audit_events_y([0-9]{4})m([0-9]{2})$');
        CONTINUE WHEN v_match IS NULL;
        v_upper := ((make_date(v_match[1]::integer, v_match[2]::integer, 1) + interval '1 month')::date)::timestamp
                   AT TIME ZONE 'UTC';
        IF v_upper <= p_now - make_interval(days => p_retention_days) THEN
            EXECUTE format('DROP TABLE %I', v_rec.relname);
            v_dropped := v_dropped + 1;
        END IF;
    END LOOP;
    RETURN v_dropped;
END
$$;

-- The current month and the lookahead exist from the first migration on.
SELECT ensure_audit_partitions(now(), 2);
