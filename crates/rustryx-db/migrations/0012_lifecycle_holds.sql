-- Lifecycle holds. Lookup only: `rx_active_legal_hold` names an active legal hold.
-- Delete and overwrite guards are not attached here.

CREATE TABLE lifecycle_holds (
    id            uuid PRIMARY KEY,
    kind          text        NOT NULL CHECK (kind IN ('legal', 'release', 'promotion')),
    component_id  uuid        REFERENCES components (id) ON DELETE CASCADE,
    repository_id uuid        REFERENCES repositories (id) ON DELETE CASCADE,
    project_id    uuid        REFERENCES projects (id) ON DELETE CASCADE,
    reason        text        NOT NULL CHECK (length(reason) BETWEEN 1 AND 500),
    created_by    uuid,
    created_at    timestamptz NOT NULL DEFAULT now(),
    expires_at    timestamptz,
    released_at   timestamptz,
    released_by   uuid,
    CONSTRAINT ck_holds_one_target CHECK (
        num_nonnulls(component_id, repository_id, project_id) = 1)
);

CREATE INDEX ix_holds_component
    ON lifecycle_holds (component_id)
    WHERE released_at IS NULL AND component_id IS NOT NULL;
CREATE INDEX ix_holds_repository
    ON lifecycle_holds (repository_id)
    WHERE released_at IS NULL AND repository_id IS NOT NULL;
CREATE INDEX ix_holds_project
    ON lifecycle_holds (project_id)
    WHERE released_at IS NULL AND project_id IS NOT NULL;

-- One active legal hold on the component, its repository, or that repository's project.
-- Lowest id wins. A released or expired hold is ignored.
CREATE FUNCTION rx_active_legal_hold(p_repository uuid, p_component uuid)
RETURNS uuid
LANGUAGE sql
STABLE
AS $$
    SELECT hold.id
      FROM repositories AS repo
      JOIN lifecycle_holds AS hold
        ON hold.kind = 'legal'
       AND hold.released_at IS NULL
       AND (hold.expires_at IS NULL OR hold.expires_at > now())
       AND (
            hold.component_id = p_component
         OR hold.repository_id = repo.id
         OR hold.project_id = repo.project_id)
     WHERE repo.id = p_repository
     ORDER BY hold.id
     LIMIT 1
$$;
