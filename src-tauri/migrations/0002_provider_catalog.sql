-- no-transaction
-- Router Desktop schema v2：供应商目录扩展到 133 家（对齐 9Router 目录）。
--
-- 1) providers.grp 的 CHECK 增加 'apikey'（第四组「API Key 提供商」）。SQLite 无法修改 CHECK，只能按官方
--    「12 步」流程重建表：必须在事务之外先关闭外键，重建完成后再打开，否则 DROP TABLE 会级联删除
--    connections / models / provider_headers，导致用户账户丢失。所以本文件声明 no-transaction，
--    并自行用 BEGIN/COMMIT 包住全部改动（失败则整体回滚，不会出现半成品）。
-- 2) 新增目录元数据列：category / service_kinds_json / website / color / text_icon / auth_modes_json /
--    pending_reason / base_url。
-- 3) 旧版 33 家内置提供商的 id 与 9Router 目录 id 不同（如 nvidia-nim -> nvidia），这里把已有连接、
--    模型、请求头、用量与媒体历史全部改指向新 id，账户一个不丢。
--
-- 迁移规则同 0001：已发布后禁止修改；账户相关表永不 DROP（providers 的重建是"建新表、搬数据、改名"）。

PRAGMA foreign_keys = OFF;
BEGIN IMMEDIATE;

CREATE TEMP TABLE legacy_provider_ids (
    old_id TEXT PRIMARY KEY,
    new_id TEXT NOT NULL
);
INSERT INTO legacy_provider_ids (old_id, new_id) VALUES
    ('claude-code', 'claude'),
    ('openai-codex', 'codex'),
    ('github-copilot', 'github'),
    ('cursor-ide', 'cursor'),
    ('kilo-code', 'kilocode'),
    ('codebuddy', 'codebuddy-intl'),
    ('muse-meta', 'muse'),
    ('zai-glm-coding', 'glm'),
    ('xai-grok', 'xai'),
    ('opencode-free', 'opencode'),
    ('kiro-ai', 'kiro'),
    ('nvidia-nim', 'nvidia'),
    ('ollama-cloud', 'ollama'),
    ('vertex-ai', 'vertex'),
    ('cloudflare', 'cloudflare-ai'),
    ('byteplus-modelark', 'byteplus');

-- 子表先改指向（只改旧版内置条目；用户自建的 custom 提供商即使 id 撞名也不动）
UPDATE connections SET provider_id = (SELECT new_id FROM legacy_provider_ids WHERE old_id = connections.provider_id)
 WHERE provider_id IN (SELECT old_id FROM legacy_provider_ids)
   AND EXISTS (SELECT 1 FROM providers p WHERE p.id = connections.provider_id AND p.kind = 'builtin');
UPDATE models SET provider_id = (SELECT new_id FROM legacy_provider_ids WHERE old_id = models.provider_id)
 WHERE provider_id IN (SELECT old_id FROM legacy_provider_ids)
   AND EXISTS (SELECT 1 FROM providers p WHERE p.id = models.provider_id AND p.kind = 'builtin');
UPDATE provider_headers SET provider_id = (SELECT new_id FROM legacy_provider_ids WHERE old_id = provider_headers.provider_id)
 WHERE provider_id IN (SELECT old_id FROM legacy_provider_ids)
   AND EXISTS (SELECT 1 FROM providers p WHERE p.id = provider_headers.provider_id AND p.kind = 'builtin');
UPDATE usage_requests SET provider_id = (SELECT new_id FROM legacy_provider_ids WHERE old_id = usage_requests.provider_id)
 WHERE provider_id IN (SELECT old_id FROM legacy_provider_ids)
   AND EXISTS (SELECT 1 FROM providers p WHERE p.id = usage_requests.provider_id AND p.kind = 'builtin');
UPDATE media_jobs SET provider_id = (SELECT new_id FROM legacy_provider_ids WHERE old_id = media_jobs.provider_id)
 WHERE provider_id IN (SELECT old_id FROM legacy_provider_ids)
   AND EXISTS (SELECT 1 FROM providers p WHERE p.id = media_jobs.provider_id AND p.kind = 'builtin');

CREATE TABLE providers_new (
    id               TEXT PRIMARY KEY,
    kind             TEXT NOT NULL CHECK (kind IN ('builtin', 'custom')),
    display_name     TEXT NOT NULL,
    grp              TEXT NOT NULL CHECK (grp IN ('custom', 'oauth', 'free', 'apikey')),
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
    updated_at       TEXT NOT NULL,
    -- 目录元数据（内置条目由启动时的幂等 seed 刷新；自定义提供商在创建时写入）
    category         TEXT,
    service_kinds_json TEXT NOT NULL DEFAULT '["llm"]',
    website          TEXT,
    color            TEXT,
    text_icon        TEXT,
    auth_modes_json  TEXT NOT NULL DEFAULT '["apiKey"]',
    -- adapter_ready=0 时向用户展示的"待接入"原因
    pending_reason   TEXT,
    -- 自定义提供商的上游地址；内置条目的地址来自 resources/provider_status.json
    base_url         TEXT
);

INSERT INTO providers_new (
    id, kind, display_name, grp, adapter, adapter_ready, protocols_json, capabilities_json, risk_notice,
    enabled, sort_order, round_robin, max_concurrency, max_retries, thinking_level, created_at, updated_at
)
SELECT
    COALESCE((SELECT new_id FROM legacy_provider_ids WHERE old_id = p.id AND p.kind = 'builtin'), p.id),
    p.kind, p.display_name, p.grp, p.adapter, p.adapter_ready, p.protocols_json, p.capabilities_json, p.risk_notice,
    p.enabled, p.sort_order, p.round_robin, p.max_concurrency, p.max_retries, p.thinking_level, p.created_at, p.updated_at
FROM providers p;

DROP TABLE providers;
ALTER TABLE providers_new RENAME TO providers;
DROP TABLE legacy_provider_ids;

COMMIT;
PRAGMA foreign_keys = ON;
