-- Routing rules of proxy repositories (core gaps 19 and 27).
--
-- A separate file from 0007 because the design keeps `routing_rules` out of the foundation
-- (wave2-design.md section 1.5, last paragraph): it is referenced by `repositories` only, not by
-- several streams. Expand only.

-- A CHECK cannot hold a subquery, a function can.
CREATE FUNCTION routing_patterns_valid(p_patterns text[]) RETURNS boolean
LANGUAGE sql IMMUTABLE AS $$
    SELECT cardinality(p_patterns) BETWEEN 1 AND 32
       AND NOT EXISTS (
           SELECT 1 FROM unnest(p_patterns) AS x WHERE x IS NULL OR x = '' OR octet_length(x) > 256)
$$;

CREATE TABLE routing_rules (
    id          uuid PRIMARY KEY,
    name        text        NOT NULL CHECK (name <> '' AND length(name) <= 128),
    mode        text        NOT NULL CHECK (mode IN ('allow', 'block')),
    -- 1 to 32 patterns of at most 256 bytes: the shape limits of `RoutingRule::validate`. A rule
    -- without a pattern would match nothing. Whether a pattern compiles is the service's check
    -- (core has no regex engine).
    patterns    text[]      NOT NULL,
    description text        NOT NULL DEFAULT '',
    version     bigint      NOT NULL DEFAULT 1,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_routing_rules_name UNIQUE (name),
    CONSTRAINT ck_routing_rules_patterns CHECK (routing_patterns_valid(patterns))
);

-- `repositories.routing_rule_id` was a bare id until now. NOT VALID: the constraint is checked
-- for every new and changed value but not against rows written before the table existed (there
-- can be no valid reference among them), so the migration cannot fail on old data and does not
-- scan the table. RESTRICT: a rule in use cannot be deleted from under its repositories.
ALTER TABLE repositories
    ADD CONSTRAINT fk_repositories_routing_rule
    FOREIGN KEY (routing_rule_id) REFERENCES routing_rules (id) ON DELETE RESTRICT NOT VALID;
CREATE INDEX ix_repositories_routing_rule ON repositories (routing_rule_id) WHERE routing_rule_id IS NOT NULL;
