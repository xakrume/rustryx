-- Retention policies (HB-20). At most one policy per project or repository.
-- Rules say what to keep. The job that deletes the rest is a later piece.

CREATE TABLE retention_policies (
    id uuid PRIMARY KEY,
    project_id uuid REFERENCES projects (id) ON DELETE CASCADE,
    repository_id uuid REFERENCES repositories (id) ON DELETE CASCADE,
    name text NOT NULL CHECK (name <> '' AND length(name) <= 128),
    rules jsonb NOT NULL CHECK (
        jsonb_typeof(rules) = 'array' AND jsonb_array_length(rules) BETWEEN 1 AND 15),
    allow_delete_last_version boolean NOT NULL DEFAULT false,
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT ck_retention_scope CHECK ((project_id IS NULL) <> (repository_id IS NULL))
);

CREATE UNIQUE INDEX uq_retention_project
    ON retention_policies (project_id) WHERE project_id IS NOT NULL;
CREATE UNIQUE INDEX uq_retention_repo
    ON retention_policies (repository_id) WHERE repository_id IS NOT NULL;
