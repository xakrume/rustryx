-- Daily download counters. Partitioned by UTC month. A deleted asset leaves its rows; they age
-- out with the partition, so the flush does not take a lock against deletes.

CREATE TABLE asset_downloads_daily (
    asset_id uuid   NOT NULL,
    day      date   NOT NULL,
    count    bigint NOT NULL CHECK (count > 0),
    PRIMARY KEY (asset_id, day)
) PARTITION BY RANGE (day);

CREATE FUNCTION download_partition_name(p_month date) RETURNS text
LANGUAGE sql IMMUTABLE AS $$
    SELECT 'asset_downloads_daily_y' || to_char(p_month, 'YYYY') || 'm' || to_char(p_month, 'MM')
$$;

CREATE FUNCTION ensure_download_partitions(
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
    PERFORM pg_advisory_xact_lock(hashtextextended('rustryx.download_partitions', 0));
    FOR i IN 0..p_lookahead_months LOOP
        v_month := (v_first + make_interval(months => i))::date;
        v_name := download_partition_name(v_month);
        IF to_regclass(quote_ident(v_name)) IS NULL THEN
            EXECUTE format(
                'CREATE TABLE %I PARTITION OF asset_downloads_daily FOR VALUES FROM (%L) TO (%L)',
                v_name,
                v_month,
                (v_month + interval '1 month')::date
            );
            v_created := v_created + 1;
        END IF;
    END LOOP;
    RETURN v_created;
END
$$;

SELECT ensure_download_partitions(now(), 2);
