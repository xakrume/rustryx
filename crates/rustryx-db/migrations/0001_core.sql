-- Foundation: extensions, key/value system configuration and the schema contract.
--
-- Migrations are forward-only and each file runs in one transaction (sqlx default).
-- They follow expand/contract: a release may only ADD columns, tables and indexes
-- while the previous binary is still serving. Anything the previous binary needs
-- (a dropped column, a stricter constraint) is removed in a LATER release, and
-- that migration must bump schema_contract.min_binary_schema (see below).

-- pg_trgm is a "trusted" extension since PostgreSQL 13, so the database owner can
-- create it without superuser rights.
CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TABLE system_config (
    key        text PRIMARY KEY CHECK (key <> '' AND length(key) <= 200),
    value      jsonb       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Single-row table read by schema_status(). A binary built against schema version E
-- keeps serving while the database is ahead of E, as long as E >= min_binary_schema.
-- Expand migrations never touch it. A contract migration (drops something an older
-- binary reads) sets it to its own version, which turns older binaries not-ready
-- instead of letting them fail on queries.
CREATE TABLE schema_contract (
    id                boolean PRIMARY KEY DEFAULT true CHECK (id),
    min_binary_schema bigint NOT NULL CHECK (min_binary_schema >= 1)
);
INSERT INTO schema_contract (id, min_binary_schema) VALUES (true, 1);
