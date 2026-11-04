-- Trust material and the policy that names it. One policy per scope.
-- The most specific scope wins as a whole. Material ids are checked by the
-- repository: an array cannot carry a foreign key.

CREATE TABLE trust_material (
    id uuid PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('cosign_public_key', 'cosign_root', 'notation_ca')),
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 128),
    pem text NOT NULL CHECK (
        octet_length(pem) <= 65536 AND position('PRIVATE KEY' in pem) = 0
    ),
    fingerprint_sha256 text NOT NULL CHECK (fingerprint_sha256 ~ '^[0-9a-f]{64}$'),
    not_after timestamptz,
    created_by uuid,
    created_at timestamptz NOT NULL
);

CREATE UNIQUE INDEX uq_trust_material_name ON trust_material (name);
CREATE UNIQUE INDEX uq_trust_material_fingerprint ON trust_material (kind, fingerprint_sha256);

CREATE TABLE trust_policies (
    id uuid PRIMARY KEY,
    project_id uuid REFERENCES projects (id) ON DELETE CASCADE,
    repository_id uuid REFERENCES repositories (id) ON DELETE CASCADE,
    system boolean NOT NULL DEFAULT false,
    required text NOT NULL CHECK (required IN ('none', 'cosign', 'notation', 'both')),
    mode text NOT NULL CHECK (mode IN ('enforce', 'audit_only')),
    cosign_key_ids uuid[] NOT NULL DEFAULT '{}',
    cosign_root_ids uuid[] NOT NULL DEFAULT '{}',
    cosign_identities jsonb NOT NULL DEFAULT '[]'::jsonb
        CHECK (jsonb_typeof(cosign_identities) = 'array'),
    cosign_require_tlog boolean NOT NULL DEFAULT false,
    notation_store_ids uuid[] NOT NULL DEFAULT '{}',
    notation_identities text[] NOT NULL DEFAULT '{}',
    notation_level text NOT NULL CHECK (notation_level IN ('strict', 'permissive', 'audit')),
    verify_children boolean NOT NULL DEFAULT true,
    version bigint NOT NULL DEFAULT 1,
    CONSTRAINT ck_trust_policy_scope CHECK (
        num_nonnulls(project_id, repository_id) + (system::int) = 1
    )
);

CREATE UNIQUE INDEX uq_trust_policy_system ON trust_policies (system) WHERE system;
CREATE UNIQUE INDEX uq_trust_policy_project ON trust_policies (project_id) WHERE project_id IS NOT NULL;
CREATE UNIQUE INDEX uq_trust_policy_repo ON trust_policies (repository_id) WHERE repository_id IS NOT NULL;
