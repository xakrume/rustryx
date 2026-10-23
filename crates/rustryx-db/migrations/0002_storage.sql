-- Blob stores, repositories, blobs, components and assets.

CREATE TABLE blob_stores (
    id          uuid PRIMARY KEY,
    name        text        NOT NULL CHECK (name <> '' AND length(name) <= 128),
    kind        text        NOT NULL CHECK (kind IN ('local', 's3')),
    config      jsonb       NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(config) = 'object'),
    quota_bytes bigint      CHECK (quota_bytes IS NULL OR quota_bytes >= 0),
    created_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_blob_stores_name UNIQUE (name)
);

CREATE TABLE repositories (
    id                 uuid PRIMARY KEY,
    name               text        NOT NULL CHECK (name <> '' AND length(name) <= 128),
    -- Shape check instead of a list: a new format (phase 2) must not need a migration.
    -- The domain enum rejects unknown labels when a row is read.
    format             text        NOT NULL CHECK (format ~ '^[a-z][a-z0-9_]{0,31}$'),
    repo_type          text        NOT NULL CHECK (repo_type IN ('hosted', 'proxy', 'group')),
    -- RESTRICT: a blob store that still backs a repository cannot disappear.
    blob_store_id      uuid        NOT NULL REFERENCES blob_stores (id) ON DELETE RESTRICT,
    write_policy       text        NOT NULL CHECK (write_policy IN ('allow', 'allow_once', 'deny')),
    allow_anonymous    boolean     NOT NULL DEFAULT false,
    online             boolean     NOT NULL DEFAULT true,
    quota_bytes        bigint      CHECK (quota_bytes IS NULL OR quota_bytes >= 0),
    cleanup_policy_ids uuid[]      NOT NULL DEFAULT '{}',
    routing_rule_id    uuid,
    proxy_config       jsonb       CHECK (jsonb_typeof(proxy_config) = 'object'),
    group_config       jsonb       CHECK (jsonb_typeof(group_config) = 'object'),
    format_config      jsonb       NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(format_config) = 'object'),
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_repositories_name UNIQUE (name),
    -- The type decides which config exists. A proxy without a remote or a hosted
    -- repository carrying a group member list would be silently ignored later.
    CONSTRAINT ck_repositories_type_config CHECK (
        (repo_type = 'proxy') = (proxy_config IS NOT NULL)
        AND (repo_type = 'group') = (group_config IS NOT NULL)
    )
);
CREATE INDEX ix_repositories_blob_store ON repositories (blob_store_id);

-- Blobs are immutable. Re-publishing a path inserts a NEW blob and repoints the asset,
-- so a download that is still streaming the old object never sees its bytes change.
CREATE TABLE blobs (
    id            uuid PRIMARY KEY,
    blob_store_id uuid        NOT NULL REFERENCES blob_stores (id) ON DELETE RESTRICT,
    key           text        NOT NULL CHECK (key <> '' AND length(key) <= 512),
    size_bytes    bigint      NOT NULL CHECK (size_bytes >= 0),
    sha1          text        NOT NULL CHECK (sha1 ~ '^[0-9a-f]{40}$'),
    sha256        text        NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    md5           text        NOT NULL CHECK (md5 ~ '^[0-9a-f]{32}$'),
    created_at    timestamptz NOT NULL DEFAULT now(),
    -- When the last asset stopped referencing the blob; NULL while referenced or never
    -- referenced. Maintained by trigger assets_blob_ref. GC compares THIS instant with
    -- its minimum age, not created_at: an old blob that was replaced a second ago may
    -- still be streamed to a client, and deleting it then would cut that download.
    orphaned_at   timestamptz,
    CONSTRAINT uq_blobs_store_key UNIQUE (blob_store_id, key)
);
CREATE INDEX ix_blobs_orphaned_at ON blobs (orphaned_at) WHERE orphaned_at IS NOT NULL;
CREATE INDEX ix_blobs_created_at ON blobs (created_at);

CREATE TABLE components (
    id            uuid PRIMARY KEY,
    repository_id uuid        NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    format        text        NOT NULL CHECK (format ~ '^[a-z][a-z0-9_]{0,31}$'),
    group_id      text,
    name          text        NOT NULL CHECK (name <> ''),
    version       text,
    -- Punctuation becomes a word break before tokenizing, so "com.google.guava" and
    -- "my-lib-1.2.3" are searchable by their parts; the 'simple' configuration avoids
    -- stemming, which would mangle package names.
    search        tsvector GENERATED ALWAYS AS (
        to_tsvector(
            'simple',
            regexp_replace(
                coalesce(group_id, '') || ' ' || name || ' ' || coalesce(version, ''),
                '[^[:alnum:]]+', ' ', 'g'
            )
        )
    ) STORED,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    -- NULLS NOT DISTINCT (PostgreSQL 15+): two components without a group or without a
    -- version are the SAME coordinate. A plain UNIQUE treats NULLs as distinct and would
    -- let concurrent uploads create duplicates of "left-pad" with no namespace.
    CONSTRAINT uq_components_coordinate
        UNIQUE NULLS NOT DISTINCT (repository_id, format, group_id, name, version)
);
CREATE INDEX ix_components_search ON components USING gin (search);
CREATE INDEX ix_components_name_trgm ON components USING gin (name gin_trgm_ops);
CREATE INDEX ix_components_group_trgm ON components USING gin (group_id gin_trgm_ops);
CREATE INDEX ix_components_repo_name ON components (repository_id, name);

CREATE TABLE assets (
    id                 uuid PRIMARY KEY,
    repository_id      uuid        NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    component_id       uuid        REFERENCES components (id) ON DELETE CASCADE,
    -- COLLATE "C": byte order is the same on every host, a plain btree serves both
    -- LIKE 'prefix%' and the keyset predicate path > $after, and ordering cannot change
    -- under a glibc/ICU upgrade (which would corrupt a locale-collated unique index).
    path               text COLLATE "C" NOT NULL CHECK (path <> '' AND length(path) <= 2048),
    -- RESTRICT: a referenced blob row cannot be deleted from under an asset.
    blob_id            uuid        NOT NULL REFERENCES blobs (id) ON DELETE RESTRICT,
    content_type       text        NOT NULL,
    last_downloaded_at timestamptz,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_assets_repo_path UNIQUE (repository_id, path)
);
CREATE INDEX ix_assets_blob ON assets (blob_id);
CREATE INDEX ix_assets_component ON assets (component_id) WHERE component_id IS NOT NULL;

CREATE FUNCTION assets_blob_ref() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('INSERT', 'UPDATE') THEN
        UPDATE blobs SET orphaned_at = NULL WHERE id = NEW.blob_id AND orphaned_at IS NOT NULL;
    END IF;
    IF (TG_OP = 'UPDATE' AND OLD.blob_id <> NEW.blob_id) OR TG_OP = 'DELETE' THEN
        UPDATE blobs SET orphaned_at = now()
        WHERE id = OLD.blob_id
          AND NOT EXISTS (SELECT 1 FROM assets a WHERE a.blob_id = OLD.blob_id);
    END IF;
    RETURN NULL;
END
$$;

-- A trigger and not application code, because cascades (deleting a repository or a
-- component removes assets without going through any repository method) must mark
-- their blobs too.
CREATE TRIGGER assets_blob_ref
    AFTER INSERT OR UPDATE OF blob_id OR DELETE ON assets
    FOR EACH ROW EXECUTE FUNCTION assets_blob_ref();
