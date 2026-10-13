-- Router Desktop schema v1 (PRODUCT_DESIGN.md §6)
-- 升级只通过追加新的迁移文件完成，禁止修改已发布迁移；账户相关表(providers/connections/models)
-- 永不在迁移中 DROP。

CREATE TABLE providers (
    id               TEXT PRIMARY KEY,
    kind             TEXT NOT NULL CHECK (kind IN ('builtin', 'custom')),
    display_name     TEXT NOT NULL,
    grp              TEXT NOT NULL CHECK (grp IN ('custom', 'oauth', 'free')),
    adapter          TEXT NOT NULL DEFAULT 'none',
    adapter_ready    INTEGER NOT NULL DEFAULT 0,
    protocols_json   TEXT NOT NULL DEFAULT '[]',
    capabilities_json TEXT NOT NULL DEFAULT '[]',
    risk_notice      TEXT,
    enabled          INTEGER NOT NULL DEFAULT 1,
    sort_order       INTEGER NOT NULL DEFAULT 0,
    round_robin      INTEGER NOT NULL DEFAULT 1,
    max_concurrency  INTEGER NOT NULL DEFAULT 4,
    max_retries      INTEGER NOT NULL DEFAULT 2,
    thinking_level   TEXT NOT NULL DEFAULT 'auto' CHECK (thinking_level IN ('auto', 'low', 'medium', 'high')),
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);

CREATE TABLE connections (
    id             TEXT PRIMARY KEY,
    provider_id    TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    account_label  TEXT NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN ('oauth', 'apiKey')),
    status         TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'paused', 'error')),
    priority       INTEGER NOT NULL DEFAULT 0,
    weight         INTEGER NOT NULL DEFAULT 1,
    enabled        INTEGER NOT NULL DEFAULT 1,
    -- 仅保存系统凭据库中的引用，不保存令牌/密钥本身
    credential_ref TEXT,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);
CREATE INDEX idx_connections_provider ON connections(provider_id);

CREATE TABLE models (
    id                TEXT PRIMARY KEY,
    provider_id       TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    upstream_model_id TEXT NOT NULL,
    public_alias      TEXT NOT NULL,
    description       TEXT NOT NULL DEFAULT '',
    supports_thinking INTEGER NOT NULL DEFAULT 0,
    capabilities_json TEXT NOT NULL DEFAULT '{}',
    enabled           INTEGER NOT NULL DEFAULT 1,
    position          INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_models_provider ON models(provider_id);
CREATE INDEX idx_models_alias ON models(public_alias);

CREATE TABLE provider_headers (
    provider_id    TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    header_key     TEXT NOT NULL,
    -- 秘密头的 value 恒为 NULL，真实值在凭据库 credential_ref 下
    value          TEXT,
    secret         INTEGER NOT NULL DEFAULT 0,
    credential_ref TEXT,
    position       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (provider_id, header_key)
);

CREATE TABLE api_keys (
    id                  TEXT PRIMARY KEY,
    name                TEXT NOT NULL,
    -- SHA-256(完整密钥) 的小写十六进制；完整密钥从不落库
    key_hash            TEXT NOT NULL,
    -- 仅用于展示掩码
    key_suffix          TEXT NOT NULL,
    protocols_json      TEXT NOT NULL DEFAULT '[]',
    allowed_models_json TEXT NOT NULL DEFAULT '[]',
    rpm_limit           INTEGER,
    tpm_limit           INTEGER,
    total_limit         INTEGER,
    call_count          INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL,
    last_used_at        TEXT,
    expires_at          TEXT,
    disabled_at         TEXT
);

-- 用量记录是历史事实，不对 provider/connection/api_key 建外键，删除账户后仍可统计
CREATE TABLE usage_requests (
    id               TEXT PRIMARY KEY,
    api_key_id       TEXT,
    provider_id      TEXT NOT NULL,
    connection_id    TEXT NOT NULL DEFAULT '',
    model_id         TEXT NOT NULL,
    protocol         TEXT NOT NULL DEFAULT '',
    status           TEXT NOT NULL CHECK (status IN ('success', 'error', 'cancelled')),
    input_tokens     INTEGER NOT NULL DEFAULT 0,
    output_tokens    INTEGER NOT NULL DEFAULT 0,
    cached_tokens    INTEGER NOT NULL DEFAULT 0,
    estimated_cost   REAL NOT NULL DEFAULT 0,
    duration_ms      INTEGER NOT NULL DEFAULT 0,
    token_source     TEXT NOT NULL DEFAULT 'upstream' CHECK (token_source IN ('upstream', 'estimated')),
    created_at       TEXT NOT NULL
);
CREATE INDEX idx_usage_created_provider_model ON usage_requests(created_at, provider_id, model_id);

CREATE TABLE quota_snapshots (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    connection_id TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    -- period: 周期额度；balance: 货币余额(unit=币种, remaining=金额)；error: 最近一次刷新失败
    kind          TEXT NOT NULL DEFAULT 'period' CHECK (kind IN ('period', 'balance', 'error')),
    quota_name    TEXT NOT NULL,
    unit          TEXT NOT NULL DEFAULT '',
    used          REAL,
    total         REAL,
    remaining     REAL,
    resets_at     TEXT,
    checked_at    TEXT NOT NULL,
    source        TEXT NOT NULL DEFAULT 'unknown' CHECK (source IN ('live', 'cached', 'estimated', 'unknown')),
    error         TEXT
);
CREATE INDEX idx_quota_connection_checked ON quota_snapshots(connection_id, checked_at);

CREATE TABLE media_jobs (
    id                  TEXT PRIMARY KEY,
    kind                TEXT NOT NULL CHECK (kind IN ('image', 'asr', 'tts', 'video', 'search')),
    provider_id         TEXT NOT NULL DEFAULT '',
    model_id            TEXT NOT NULL DEFAULT '',
    summary             TEXT NOT NULL DEFAULT '',
    -- 只存元数据；大文件只存 output_path，不存 BLOB
    input_metadata_json TEXT NOT NULL DEFAULT '{}',
    output_path         TEXT,
    status              TEXT NOT NULL CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    error               TEXT,
    created_at          TEXT NOT NULL,
    finished_at         TEXT
);
CREATE INDEX idx_media_kind_created ON media_jobs(kind, created_at);

CREATE TABLE app_settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
