//! Claude Code：`~/.claude/settings.json` 的 `env`。
//!
//! `ANTHROPIC_BASE_URL` 不带 /v1；`ANTHROPIC_AUTH_TOKEN` 以 Bearer 发送。后台任务会请求 Haiku 档位，
//! 所以三个档位模型都指向所选模型。依赖 serde_json 的 preserve_order，原文件的键顺序不会被打乱。

use serde_json::{Map, Value};

use super::{mask, FieldChange, Params, Plan};

pub(crate) fn plan(existing: Option<&str>, p: &Params) -> Result<Plan, String> {
    let original: Option<Value> = match existing.map(str::trim) {
        None | Some("") => None,
        Some(text) => Some(serde_json::from_str(text).map_err(|e| {
            // 只报位置，不回显内容
            format!("现有 settings.json 不是合法的 JSON（第 {} 行第 {} 列），为避免覆盖已拒绝合并。请先修正该文件。", e.line(), e.column())
        })?),
    };
    let mut root = original.clone().unwrap_or_else(|| Value::Object(Map::new()));
    let obj = root.as_object_mut().ok_or("现有 settings.json 的顶层不是对象，无法自动合并。")?;
    let env = obj
        .entry("env")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("现有 settings.json 里的 env 不是对象，无法自动合并。")?;

    let fields: [(&str, &str, bool); 6] = [
        ("ANTHROPIC_BASE_URL", p.origin.as_str(), false),
        ("ANTHROPIC_AUTH_TOKEN", p.key.as_str(), true),
        ("ANTHROPIC_MODEL", p.model.as_str(), false),
        ("ANTHROPIC_DEFAULT_HAIKU_MODEL", p.model.as_str(), false),
        ("ANTHROPIC_DEFAULT_SONNET_MODEL", p.model.as_str(), false),
        ("ANTHROPIC_DEFAULT_OPUS_MODEL", p.model.as_str(), false),
    ];
    let mut changes = Vec::new();
    for (key, new, secret) in fields {
        let before = env.get(key).and_then(Value::as_str).map(|b| if secret { mask(b) } else { b.to_string() });
        changes.push(FieldChange { field: format!("env.{key}"), before, after: Some(new.into()) });
        env.insert(key.to_string(), Value::String(new.to_string()));
    }

    let mut content = serde_json::to_string_pretty(&root).map_err(|e| format!("生成 settings.json 失败：{e}"))?;
    content.push('\n');
    // 比较语义而不是文本：用户的缩进风格不同不算“有变化”
    let unchanged = original.as_ref() == Some(&root);
    Ok(Plan { content, changes, unchanged })
}
