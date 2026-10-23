-- Built-in roles and privileges, then the triggers that make them read-only.
--
-- Order matters: the seed rows are inserted BEFORE the guards exist, because the guard on
-- role_privileges rejects every write that touches a built-in role. A later migration that
-- must change a built-in role has to disable the guard inside its own transaction:
--   ALTER TABLE role_privileges DISABLE TRIGGER role_privileges_builtin_guard;
-- (and enable it again before the end of the file).
--
-- There is deliberately NO seeded user and NO default password. The first administrator
-- is created by bootstrap_admin() from a secret supplied at deploy time.

INSERT INTO privileges (id, name, description, builtin, grant_kind, grant_repository, actions) VALUES
    ('01900000-0000-7000-8000-000000000001', 'nx-all',
     'Every action on everything, including administration', true,
     'admin', NULL, ARRAY['browse', 'read', 'write', 'delete']),
    ('01900000-0000-7000-8000-000000000002', 'nx-repository-view-all-read',
     'Browse and read every repository', true,
     'repository-view', '*', ARRAY['browse', 'read']),
    ('01900000-0000-7000-8000-000000000003', 'nx-repository-view-all-readwrite',
     'Browse, read and write every repository', true,
     'repository-view', '*', ARRAY['browse', 'read', 'write']);

INSERT INTO roles (id, name, description, builtin) VALUES
    ('01900000-0000-7000-8000-000000000011', 'nx-admin',
     'Administrator. Assigned only by exact match of a configured admin group or by bootstrap', true),
    ('01900000-0000-7000-8000-000000000012', 'nx-anonymous',
     'Unauthenticated callers. Read and browse only, and only where the global switch AND the repository flag allow it', true),
    ('01900000-0000-7000-8000-000000000013', 'nx-developer',
     'Read and write on repositories', true);

INSERT INTO role_privileges (role_id, privilege_id) VALUES
    ('01900000-0000-7000-8000-000000000011', '01900000-0000-7000-8000-000000000001'),
    ('01900000-0000-7000-8000-000000000012', '01900000-0000-7000-8000-000000000002'),
    ('01900000-0000-7000-8000-000000000013', '01900000-0000-7000-8000-000000000003');

CREATE FUNCTION reject_builtin_row_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.builtin THEN
        RAISE EXCEPTION 'built-in row is read-only' USING ERRCODE = 'RX002';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER roles_builtin_guard BEFORE UPDATE OR DELETE ON roles
    FOR EACH ROW EXECUTE FUNCTION reject_builtin_row_change();
CREATE TRIGGER privileges_builtin_guard BEFORE UPDATE OR DELETE ON privileges
    FOR EACH ROW EXECUTE FUNCTION reject_builtin_row_change();

-- The role-side tables are guarded too: attaching a privilege to nx-anonymous would be an
-- edit of the built-in role even though the roles row itself never changes.
CREATE FUNCTION reject_builtin_membership_change() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    v_role uuid;
    v_builtin boolean;
BEGIN
    IF TG_TABLE_NAME = 'role_roles' THEN
        v_role := CASE WHEN TG_OP = 'DELETE' THEN OLD.parent_role_id ELSE NEW.parent_role_id END;
    ELSE
        v_role := CASE WHEN TG_OP = 'DELETE' THEN OLD.role_id ELSE NEW.role_id END;
    END IF;
    SELECT builtin INTO v_builtin FROM roles WHERE id = v_role;
    -- NULL: the role row is already gone (cascade of a non built-in role delete).
    IF coalesce(v_builtin, false) THEN
        RAISE EXCEPTION 'built-in role is read-only' USING ERRCODE = 'RX002';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER role_privileges_builtin_guard BEFORE INSERT OR UPDATE OR DELETE ON role_privileges
    FOR EACH ROW EXECUTE FUNCTION reject_builtin_membership_change();
CREATE TRIGGER role_roles_builtin_guard BEFORE INSERT OR UPDATE OR DELETE ON role_roles
    FOR EACH ROW EXECUTE FUNCTION reject_builtin_membership_change();
