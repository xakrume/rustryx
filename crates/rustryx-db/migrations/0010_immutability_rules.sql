-- Immutability rules (HB-24). A matching tag, version or path cannot be overwritten or
-- deleted. The check itself is `rx_immutable_rule`; publish guards attach to it later.

CREATE FUNCTION rx_selector_matches(p_mode text, p_regexes text[], p_value text)
RETURNS boolean
LANGUAGE sql
IMMUTABLE
AS $$
    SELECT CASE
        WHEN p_mode = 'matching' THEN EXISTS (
            SELECT 1 FROM unnest(p_regexes) AS pattern WHERE p_value ~ pattern)
        ELSE NOT EXISTS (
            SELECT 1 FROM unnest(p_regexes) AS pattern WHERE p_value ~ pattern)
    END
$$;

CREATE TABLE immutability_rules (
    id            uuid PRIMARY KEY,
    project_id    uuid REFERENCES projects (id) ON DELETE CASCADE,
    repository_id uuid REFERENCES repositories (id) ON DELETE CASCADE,
    name          text        NOT NULL CHECK (name <> '' AND length(name) <= 128),
    item_kind     text        NOT NULL CHECK (item_kind IN ('tag', 'version', 'path')),
    repo_mode     text        NOT NULL DEFAULT 'matching' CHECK (repo_mode IN ('matching', 'excluding')),
    repo_globs    text[]      NOT NULL DEFAULT '{}',
    repo_regexes  text[]      NOT NULL DEFAULT '{}',
    item_mode     text        NOT NULL DEFAULT 'matching' CHECK (item_mode IN ('matching', 'excluding')),
    item_globs    text[]      NOT NULL DEFAULT '{}',
    item_regexes  text[]      NOT NULL DEFAULT '{}',
    enabled       boolean     NOT NULL DEFAULT true,
    version       bigint      NOT NULL DEFAULT 1,
    created_by    uuid,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT ck_immutability_scope CHECK ((project_id IS NULL) <> (repository_id IS NULL)),
    CONSTRAINT ck_immutability_glob_count CHECK (
        cardinality(repo_globs) <= 16 AND cardinality(item_globs) <= 16),
    CONSTRAINT ck_immutability_matching_nonempty CHECK (
        (item_mode = 'excluding' OR cardinality(item_globs) > 0)
        AND (repo_mode = 'excluding' OR cardinality(repo_globs) > 0)),
    CONSTRAINT ck_immutability_regex_shape CHECK (
        cardinality(repo_regexes) = cardinality(repo_globs)
        AND cardinality(item_regexes) = cardinality(item_globs))
);

CREATE INDEX ix_immutability_project
    ON immutability_rules (project_id, item_kind)
    WHERE enabled AND project_id IS NOT NULL;
CREATE INDEX ix_immutability_repo
    ON immutability_rules (repository_id, item_kind)
    WHERE enabled AND repository_id IS NOT NULL;

-- At most 15 rules of one scope. The advisory lock serialises two inserts of the same scope.
CREATE FUNCTION immutability_rules_limit() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    n integer;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(
        coalesce(NEW.project_id::text, '') || ':' || coalesce(NEW.repository_id::text, ''), 0));
    SELECT count(*) INTO n
      FROM immutability_rules
     WHERE project_id IS NOT DISTINCT FROM NEW.project_id
       AND repository_id IS NOT DISTINCT FROM NEW.repository_id;
    IF n >= 15 THEN
        RAISE EXCEPTION 'a scope has at most 15 immutability rules'
            USING ERRCODE = '23514', CONSTRAINT = 'ck_immutability_max_per_scope';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER immutability_rules_limit
    BEFORE INSERT ON immutability_rules
    FOR EACH ROW EXECUTE FUNCTION immutability_rules_limit();

-- First enabled rule of the repository or of its project, in id order.
CREATE FUNCTION rx_immutable_rule(p_repository uuid, p_kind text, p_item text)
RETURNS uuid
LANGUAGE sql
STABLE
AS $$
    SELECT rule.id
      FROM repositories AS repo
      JOIN immutability_rules AS rule
        ON rule.enabled
       AND rule.item_kind = p_kind
       AND (rule.repository_id = repo.id OR rule.project_id = repo.project_id)
       AND rx_selector_matches(rule.repo_mode, rule.repo_regexes, repo.name)
       AND rx_selector_matches(rule.item_mode, rule.item_regexes, p_item)
     WHERE repo.id = p_repository
     ORDER BY rule.id
     LIMIT 1
$$;
