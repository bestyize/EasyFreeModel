-- Router Desktop schema v5：代理池节点。密码只存系统/本地凭据库，这里仅保存 credential_ref。
CREATE TABLE proxy_nodes (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    scheme         TEXT NOT NULL CHECK (scheme IN ('http', 'https', 'socks5', 'socks5h')),
    host           TEXT NOT NULL,
    port           INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    username       TEXT,
    credential_ref TEXT,
    enabled        INTEGER NOT NULL DEFAULT 1,
    sort_order     INTEGER NOT NULL DEFAULT 0,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);
