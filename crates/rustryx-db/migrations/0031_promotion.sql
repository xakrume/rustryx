-- Promotion rules, requests, the components they move and the decisions that approve them.

CREATE TABLE promotion_targets (
    repository_id uuid PRIMARY KEY REFERENCES repositories (id) ON DELETE CASCADE,
    require_approval boolean NOT NULL DEFAULT false,
    required_approvals smallint NOT NULL DEFAULT 1 CHECK (required_approvals >= 0),
    checks jsonb NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(checks) = 'array'),
    version bigint NOT NULL DEFAULT 1
);

CREATE TABLE promotion_requests (
    id uuid PRIMARY KEY,
    mode text NOT NULL CHECK (mode IN ('copy', 'move')),
    source_repository_id uuid NOT NULL REFERENCES repositories (id) ON DELETE RESTRICT,
    target_repository_id uuid NOT NULL REFERENCES repositories (id) ON DELETE RESTRICT,
    selector jsonb NOT NULL,
    requested_by uuid NOT NULL,
    requested_at timestamptz NOT NULL,
    status text NOT NULL CHECK (status IN (
        'pending_approval', 'approved', 'rejected', 'running', 'succeeded',
        'partially_failed', 'failed', 'cancelled', 'expired'
    )),
    required_approvals smallint NOT NULL DEFAULT 0,
    checks jsonb NOT NULL DEFAULT '[]'::jsonb,
    expires_at timestamptz NOT NULL,
    started_at timestamptz,
    finished_at timestamptz,
    CONSTRAINT ck_promotion_repos CHECK (source_repository_id <> target_repository_id)
);
CREATE INDEX ix_promotion_requests_created ON promotion_requests (requested_at DESC, id DESC);

CREATE TABLE promotion_items (
    promotion_id uuid NOT NULL REFERENCES promotion_requests (id) ON DELETE CASCADE,
    component_id uuid NOT NULL REFERENCES components (id) ON DELETE RESTRICT,
    status text NOT NULL CHECK (status IN ('pending', 'done', 'skipped', 'failed')),
    reason text CHECK (reason IS NULL OR length(reason) <= 64),
    PRIMARY KEY (promotion_id, component_id)
);

CREATE TABLE promotion_decisions (
    promotion_id uuid NOT NULL REFERENCES promotion_requests (id) ON DELETE CASCADE,
    approver uuid NOT NULL,
    approve boolean NOT NULL,
    comment text NOT NULL DEFAULT '' CHECK (length(comment) <= 1000),
    counted boolean NOT NULL,
    at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (promotion_id, approver)
);
