-- Blob store groups (wave2-design section 7.c). Members are rows, not JSON.
-- blob_moves and verify_runs stay out of this migration.

ALTER TABLE blob_stores DROP CONSTRAINT blob_stores_kind_check;
ALTER TABLE blob_stores ADD CONSTRAINT blob_stores_kind_check
    CHECK (kind IN ('local', 's3', 'azure', 'group'));

CREATE TABLE blob_store_members (
    group_id  uuid     NOT NULL REFERENCES blob_stores (id) ON DELETE CASCADE,
    member_id uuid     NOT NULL REFERENCES blob_stores (id) ON DELETE RESTRICT,
    position  smallint NOT NULL CHECK (position >= 0),
    state     text     NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'draining')),
    PRIMARY KEY (group_id, member_id),
    CONSTRAINT uq_member_position UNIQUE (group_id, position) DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT uq_member_single_group UNIQUE (member_id),
    CONSTRAINT ck_member_not_self CHECK (group_id <> member_id)
);

-- Group row must be kind 'group' and the member must not be a group (no nesting).
-- A missing store is left to the foreign key, which is already a conflict.
CREATE FUNCTION blob_store_members_shape() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    group_kind text;
    member_kind text;
BEGIN
    SELECT kind INTO group_kind FROM blob_stores WHERE id = NEW.group_id;
    SELECT kind INTO member_kind FROM blob_stores WHERE id = NEW.member_id;
    IF group_kind IS NULL OR member_kind IS NULL THEN
        RETURN NEW;
    END IF;
    IF group_kind <> 'group' OR member_kind = 'group' THEN
        RAISE EXCEPTION 'a group holds plain stores and a group cannot be a member'
            USING ERRCODE = 'RX008';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER blob_store_members_shape
    BEFORE INSERT OR UPDATE ON blob_store_members
    FOR EACH ROW EXECUTE FUNCTION blob_store_members_shape();
