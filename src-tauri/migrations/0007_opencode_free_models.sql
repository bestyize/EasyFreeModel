-- Existing installations: add the new presets once, preserving user models and settings.
-- Empty catalogs are populated by catalog::seed after migrations.
WITH additions(upstream, description, position) AS (VALUES
    ('deepseek-v4.1-flash-free', 'DeepSeek V4.1 Flash Free', 4),
    ('mimo-v2.6-flash-free', 'MiMo V2.6 Flash Free', 5),
    ('nemotron-3.5-super-vl-free', 'Nemotron 3.5 Super VL Free', 6)
)
INSERT OR IGNORE INTO models
    (id, provider_id, upstream_model_id, public_alias, description, supports_thinking, enabled, position)
SELECT 'opencode/' || a.upstream, 'opencode', a.upstream, 'oc/' || a.upstream,
       a.description, 0,
       CASE WHEN EXISTS (SELECT 1 FROM models WHERE public_alias = 'oc/' || a.upstream AND enabled = 1) THEN 0 ELSE 1 END,
       a.position
FROM additions a
WHERE EXISTS (SELECT 1 FROM providers WHERE id = 'opencode' AND kind = 'builtin')
  AND EXISTS (SELECT 1 FROM models WHERE provider_id = 'opencode')
  AND NOT EXISTS (SELECT 1 FROM models WHERE provider_id = 'opencode' AND upstream_model_id = a.upstream);
