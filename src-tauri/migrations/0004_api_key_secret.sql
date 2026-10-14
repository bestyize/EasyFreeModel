-- Router Desktop schema v4：在 api_keys 表中增加 key_secret 字段，允许持久化保存并在关闭后再次查看密钥
ALTER TABLE api_keys ADD COLUMN key_secret TEXT;
