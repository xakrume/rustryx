-- A legal hold blocks replace and delete the same way an immutability rule does.
-- DETAIL is the hold uuid text (parsed as PolicyViolation::Held). The immutability
-- override does not lift a hold. An update that keeps the same sha256 stays allowed.

CREATE OR REPLACE FUNCTION assets_guard() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    rule_id uuid;
    hold_id uuid;
    component_version text;
BEGIN
    IF TG_OP = 'UPDATE' AND EXISTS (
        SELECT 1
          FROM blobs AS old_blob
          JOIN blobs AS new_blob ON new_blob.sha256 = old_blob.sha256
         WHERE old_blob.id = OLD.blob_id
           AND new_blob.id = NEW.blob_id
    ) THEN
        RETURN NEW;
    END IF;

    hold_id := rx_active_legal_hold(OLD.repository_id, OLD.component_id);
    IF hold_id IS NOT NULL THEN
        RAISE EXCEPTION 'item is held'
            USING ERRCODE = 'RX005', DETAIL = hold_id::text;
    END IF;

    IF current_setting('rustryx.override_immutable', true) = 'on' THEN
        IF TG_OP = 'DELETE' THEN
            RETURN OLD;
        END IF;
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

CREATE OR REPLACE FUNCTION components_guard() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    rule_id uuid;
    hold_id uuid;
BEGIN
    hold_id := rx_active_legal_hold(OLD.repository_id, OLD.id);
    IF hold_id IS NOT NULL THEN
        RAISE EXCEPTION 'item is held'
            USING ERRCODE = 'RX005', DETAIL = hold_id::text;
    END IF;

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
