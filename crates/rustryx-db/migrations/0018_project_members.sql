-- Membership. One row per subject per project; a role change is an UPDATE.
CREATE TABLE project_members (
    id            uuid PRIMARY KEY,
    project_id    uuid        NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    subject_kind  text        NOT NULL CHECK (subject_kind IN ('user', 'group')),
    user_id       uuid        REFERENCES users (id) ON DELETE CASCADE,
    provider      text        CHECK (provider IN ('oidc', 'ldap')),
    -- Exact and case sensitive, like idp_group_mappings.external_group; never normalised.
    external_group text       CHECK (external_group <> '' AND length(external_group) <= 1024),
    role          text        NOT NULL CHECK (role IN ('limited_guest', 'guest', 'developer', 'maintainer', 'project_admin')),
    added_by      uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT ck_project_members_shape CHECK (
        (subject_kind = 'user'  AND user_id IS NOT NULL AND provider IS NULL AND external_group IS NULL) OR
        (subject_kind = 'group' AND user_id IS NULL AND provider IS NOT NULL AND external_group IS NOT NULL))
);
CREATE UNIQUE INDEX uq_project_members_user  ON project_members (project_id, user_id) WHERE subject_kind = 'user';
CREATE UNIQUE INDEX uq_project_members_group ON project_members (project_id, provider, external_group) WHERE subject_kind = 'group';
CREATE INDEX ix_project_members_user  ON project_members (user_id) WHERE subject_kind = 'user';
CREATE INDEX ix_project_members_group ON project_members (provider, external_group) WHERE subject_kind = 'group';

-- Last project_admin: lock the project, then refuse a delete or a role change that would
-- leave none, unless the caller set rustryx.allow_last_admin for this transaction.
CREATE FUNCTION project_members_last_admin() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    project uuid;
    remaining bigint;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        IF OLD.role <> 'project_admin' OR NEW.role = 'project_admin' THEN
            RETURN NEW;
        END IF;
        project := OLD.project_id;
    ELSE
        IF OLD.role <> 'project_admin' THEN
            RETURN OLD;
        END IF;
        project := OLD.project_id;
    END IF;

    PERFORM pg_advisory_xact_lock(hashtextextended(project::text, 0));

    SELECT count(*) INTO remaining
      FROM project_members
     WHERE project_id = project
       AND role = 'project_admin'
       AND id <> OLD.id;

    IF remaining = 0
       AND current_setting('rustryx.allow_last_admin', true) IS DISTINCT FROM 'on'
    THEN
        RAISE EXCEPTION 'a project needs at least one project admin'
            USING ERRCODE = 'RX008';
    END IF;

    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER project_members_last_admin
    BEFORE UPDATE OR DELETE ON project_members
    FOR EACH ROW EXECUTE FUNCTION project_members_last_admin();
