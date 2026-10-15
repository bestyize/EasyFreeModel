-- no-transaction
-- Allow connections without credentials for catalog entries explicitly marked noAuth.
-- Rebuild preserves existing accounts and quota snapshots (foreign keys disabled during swap).
PRAGMA foreign_keys = OFF;
BEGIN IMMEDIATE;

CREATE TABLE connections_new (
    id             TEXT PRIMARY KEY,
    provider_id    TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    account_label  TEXT NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN ('oauth', 'apiKey', 'none')),
    status         TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'paused', 'error', 'needsReauth')),
    priority       INTEGER NOT NULL DEFAULT 0,
    weight         INTEGER NOT NULL DEFAULT 1,
    enabled        INTEGER NOT NULL DEFAULT 1,
    credential_ref TEXT,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);

INSERT INTO connections_new (
    id, provider_id, account_label, kind, status, priority, weight, enabled, credential_ref, created_at, updated_at
)
SELECT
    id, provider_id, account_label, kind, status, priority, weight, enabled, credential_ref, created_at, updated_at
FROM connections;

DROP TABLE connections;
ALTER TABLE connections_new RENAME TO connections;
CREATE INDEX idx_connections_provider ON connections(provider_id);
CREATE UNIQUE INDEX idx_connections_no_auth_provider ON connections(provider_id) WHERE kind = 'none';

COMMIT;
PRAGMA foreign_keys = ON;
