-- CVE exceptions. An expired row stops counting at once. expiry_audited_at marks that the
-- expiry job has already recorded it, and a later change of the expiry clears the mark.

CREATE TABLE cve_allowlist (
    id uuid PRIMARY KEY,
    project_id uuid REFERENCES projects (id) ON DELETE CASCADE,
    component_id uuid REFERENCES components (id) ON DELETE CASCADE,
    system boolean NOT NULL DEFAULT false,
    vuln_id text NOT NULL CHECK (vuln_id = upper(vuln_id) AND length(vuln_id) BETWEEN 1 AND 64),
    reason text NOT NULL CHECK (length(reason) BETWEEN 1 AND 500),
    created_by uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz,
    expiry_audited_at timestamptz,
    version bigint NOT NULL DEFAULT 1,
    CONSTRAINT ck_cve_allowlist_scope CHECK (
        num_nonnulls(project_id, component_id) + (system::int) = 1
    )
);
CREATE UNIQUE INDEX uq_cve_allowlist_system ON cve_allowlist (vuln_id) WHERE system;
CREATE UNIQUE INDEX uq_cve_allowlist_project ON cve_allowlist (project_id, vuln_id)
    WHERE project_id IS NOT NULL;
CREATE UNIQUE INDEX uq_cve_allowlist_component ON cve_allowlist (component_id, vuln_id)
    WHERE component_id IS NOT NULL;
