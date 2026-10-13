-- no-transaction
-- Router Desktop schema v3：连接状态增加 'needsReauth'（OAuth 令牌失效，需要用户重新授权）。
--
-- SQLite 无法修改 CHECK 约束，只能按官方「12 步」流程重建 connections 表。与 0002 相同：重建必须在事务之外先
-- 关闭外键（否则 DROP TABLE 会级联删除引用它的 quota_snapshots），所以本文件声明 no-transaction，并自行用
-- BEGIN/COMMIT 包住全部改动（失败则整体回滚，不会出现半成品）。已有连接、凭据引用、优先级与权重原样搬入，
-- 配额快照因外键关闭不受影响。
--
-- 迁移规则同 0001/0002：已发布后禁止修改；账户相关表永不丢数据（"建新表、搬数据、改名"）。

PRAGMA foreign_keys = OFF;
BEGIN IMMEDIATE;

CREATE TABLE connections_new (
    id             TEXT PRIMARY KEY,
    provider_id    TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    account_label  TEXT NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN ('oauth', 'apiKey')),
    status         TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'paused', 'error', 'needsReauth')),
    priority       INTEGER NOT NULL DEFAULT 0,
    weight         INTEGER NOT NULL DEFAULT 1,
    enabled        INTEGER NOT NULL DEFAULT 1,
    -- 仅保存系统凭据库中的引用，不保存令牌/密钥本身
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

COMMIT;
PRAGMA foreign_keys = ON;
