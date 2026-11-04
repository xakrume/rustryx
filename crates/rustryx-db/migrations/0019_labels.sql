-- Labels. System labels have project_id NULL. A project label applies only to components
-- of a repository of that project, and a component carries at most 50 labels.

CREATE TABLE labels (
    id          uuid PRIMARY KEY,
    project_id  uuid        REFERENCES projects (id) ON DELETE CASCADE,
    name        text        NOT NULL CHECK (name ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    color       text        NOT NULL DEFAULT '#6b7280' CHECK (color ~ '^#[0-9a-f]{6}$'),
    description text        NOT NULL DEFAULT '' CHECK (length(description) <= 500),
    attributes  jsonb       NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(attributes) = 'object' AND pg_column_size(attributes) <= 20480),
    version     bigint      NOT NULL DEFAULT 1,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX uq_labels_system  ON labels (name) WHERE project_id IS NULL;
CREATE UNIQUE INDEX uq_labels_project ON labels (project_id, name) WHERE project_id IS NOT NULL;

CREATE TABLE component_labels (
    component_id uuid        NOT NULL REFERENCES components (id) ON DELETE CASCADE,
    label_id     uuid        NOT NULL REFERENCES labels (id) ON DELETE CASCADE,
    applied_by   uuid        REFERENCES users (id) ON DELETE SET NULL,
    applied_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (component_id, label_id)
);
CREATE INDEX ix_component_labels_label ON component_labels (label_id);

CREATE FUNCTION component_labels_guard() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    n integer;
    label_project uuid;
    repo_project uuid;
BEGIN
    SELECT project_id INTO label_project FROM labels WHERE id = NEW.label_id;
    IF label_project IS NOT NULL THEN
        SELECT r.project_id INTO repo_project
          FROM components AS c
          JOIN repositories AS r ON r.id = c.repository_id
         WHERE c.id = NEW.component_id;
        IF repo_project IS DISTINCT FROM label_project THEN
            RAISE EXCEPTION 'a project label can only be applied to a component of that project'
                USING ERRCODE = 'RX008';
        END IF;
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended(NEW.component_id::text, 0));
    SELECT count(*) INTO n FROM component_labels WHERE component_id = NEW.component_id;
    IF n >= 50 THEN
        RAISE EXCEPTION 'a component carries at most 50 labels'
            USING ERRCODE = 'RX009';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER component_labels_guard
    BEFORE INSERT ON component_labels
    FOR EACH ROW EXECUTE FUNCTION component_labels_guard();
