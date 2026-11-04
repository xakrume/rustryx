-- Scan settings per scope. A null field inherits: repository over project over system.
-- The system row is seeded at startup, not here. Deleting a scanner clears the pointer.

CREATE TABLE scan_policies (
    id uuid PRIMARY KEY,
    project_id uuid REFERENCES projects (id) ON DELETE CASCADE,
    repository_id uuid REFERENCES repositories (id) ON DELETE CASCADE,
    system boolean NOT NULL DEFAULT false,
    auto_scan boolean,
    scanner_id uuid REFERENCES scanners (id) ON DELETE SET NULL,
    gate jsonb CHECK (gate IS NULL OR jsonb_typeof(gate) = 'object'),
    quarantine boolean,
    allowlist_mode text CHECK (allowlist_mode IS NULL OR allowlist_mode IN ('system', 'project')),
    health_check boolean,
    sbom_on_push boolean,
    version bigint NOT NULL DEFAULT 1,
    CONSTRAINT ck_scan_policy_scope CHECK (
        num_nonnulls(project_id, repository_id) + (system::int) = 1
    )
);
CREATE UNIQUE INDEX uq_scan_policy_system ON scan_policies (system) WHERE system;
CREATE UNIQUE INDEX uq_scan_policy_project ON scan_policies (project_id) WHERE project_id IS NOT NULL;
CREATE UNIQUE INDEX uq_scan_policy_repo ON scan_policies (repository_id) WHERE repository_id IS NOT NULL;
