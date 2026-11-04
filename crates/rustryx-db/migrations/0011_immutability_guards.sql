-- HB-24: refuse replace and delete of an asset or component covered by an immutability
-- rule. A blob rewrite whose sha256 does not change is an idempotent re-push and is
-- allowed. Set rustryx.override_immutable to on to skip the guard for one session.
-- DETAIL is the rule uuid text and nothing else (parsed by map_mutation_err).

CREATE FUNCTION assets_guard() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    rule_id uuid;
    component_version text;
BEGIN
    IF current_setting('rustryx.override_immutable', true) = 'on' THEN
        IF TG_OP = 'DELETE' THEN
            RETURN OLD;
        END IF;
        RETURN NEW;
    END IF;

    -- Same digest: the bytes did not change, even when the blob row id did.
    IF TG_OP = 'UPDATE' AND EXISTS (
        SELECT 1
          FROM blobs AS old_blob
          JOIN blobs AS new_blob ON new_blob.sha256 = old_blob.sha256
         WHERE old_blob.id = OLD.blob_id
           AND new_blob.id = NEW.blob_id
    ) THEN
        RETURN NEW;
    END IF;

    rule_id := rx_immutable_rule(OLD.repository_id, 'path', OLD.path);
    IF rule_id IS NULL AND OLD.component_id IS NOT NULL THEN
        SELECT version INTO component_version
          FROM components
         WHERE id = OLD.component_id;
        rule_id := rx_immutable_rule(OLD.repository_id, 'version', component_version);
    END IF;

    IF rule_id IS NOT NULL THEN
        RAISE EXCEPTION 'item is immutable'
            USING ERRCODE = 'RX004', DETAIL = rule_id::text;
    END IF;

    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER assets_guard
    BEFORE UPDATE OF blob_id OR DELETE ON assets
    FOR EACH ROW EXECUTE FUNCTION assets_guard();

CREATE FUNCTION components_guard() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    rule_id uuid;
BEGIN
    IF current_setting('rustryx.override_immutable', true) = 'on' THEN
        RETURN OLD;
    END IF;

    rule_id := rx_immutable_rule(OLD.repository_id, 'version', OLD.version);
    IF rule_id IS NOT NULL THEN
        RAISE EXCEPTION 'item is immutable'
            USING ERRCODE = 'RX004', DETAIL = rule_id::text;
    END IF;
    RETURN OLD;
END
$$;

CREATE TRIGGER components_guard
    BEFORE DELETE ON components
    FOR EACH ROW EXECUTE FUNCTION components_guard();
