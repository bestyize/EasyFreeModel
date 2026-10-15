//! Codex：`~/.codex/config.toml`。
//!
//! 写入顶层 `model` / `model_provider`，以及 `[model_providers.router]`：
//! `wire_api` 目前只能是 "responses"；密钥直接写 `experimental_bearer_token`（本机客户端图省事，
//! 0.100 到 0.160 都支持），所以不需要再设环境变量。
//! 用 toml_edit 就地修改，注释、顺序和其它配置原样保留。

use toml_edit::{value, DocumentMut, Item, Table, Value};

use super::{mask, FieldChange, Params, Plan};

pub const PROVIDER_ID: &str = "freeapibox";
const PROVIDER_NAME: &str = "FreeApiBox";

fn parse_error(text: &str, e: &toml_edit::TomlError) -> String {
    // 只报行号和语法原因：完整错误会带上出错的那一行原文，可能含密钥
    let line = e.span().and_then(|s| text.get(..s.start)).map(|head| head.matches('\n').count() + 1);
    match line {
        Some(l) => format!("现有 config.toml 不是合法的 TOML（第 {l} 行：{}），为避免覆盖已拒绝合并。请先修正该文件。", e.message()),
        None => format!("现有 config.toml 不是合法的 TOML（{}），为避免覆盖已拒绝合并。请先修正该文件。", e.message()),
    }
}

fn str_of(item: Option<&Item>) -> Option<String> {
    item.and_then(Item::as_str).map(str::to_string)
}

/// 改值时保留原来的行内注释和空白。
fn set_str(table: &mut Table, key: &str, new: &str) {
    if let Some(Item::Value(old)) = table.get_mut(key) {
        let decor = old.decor().clone();
        let mut v = Value::from(new);
        *v.decor_mut() = decor;
        *old = v;
    } else {
        table.insert(key, value(new));
    }
}

pub(crate) fn plan(existing: Option<&str>, p: &Params) -> Result<Plan, String> {
    let mut doc: DocumentMut = match existing {
        Some(text) => text.parse().map_err(|e| parse_error(text, &e))?,
        None => DocumentMut::new(),
    };
    let mut changes = Vec::new();
    let root = doc.as_table_mut();

    for (key, new) in [("model", p.model.as_str()), ("model_provider", PROVIDER_ID)] {
        changes.push(FieldChange { field: key.into(), before: str_of(root.get(key)), after: Some(new.into()) });
        set_str(root, key, new);
    }

    let mut implicit = Table::new();
    implicit.set_implicit(true);
    let providers = root
        .entry("model_providers")
        .or_insert(Item::Table(implicit))
        .as_table_mut()
        .ok_or("现有 config.toml 里的 model_providers 不是普通表（可能是内联写法），无法自动合并。")?;
    let router = providers
        .entry(PROVIDER_ID)
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or("现有 config.toml 里的 model_providers.freeapibox 不是普通表，无法自动合并。")?;

    let base_url = format!("{}/v1", p.origin);
    let fields: [(&str, &str, bool); 4] = [
        ("name", PROVIDER_NAME, false),
        ("base_url", base_url.as_str(), false),
        ("wire_api", "responses", false),
        ("experimental_bearer_token", p.key.as_str(), true),
    ];
    for (key, new, secret) in fields {
        let before = str_of(router.get(key)).map(|b| if secret { mask(&b) } else { b });
        changes.push(FieldChange { field: format!("model_providers.{PROVIDER_ID}.{key}"), before, after: Some(new.into()) });
        set_str(router, key, new);
    }
    // 旧版接入说明用过 env_key；留着会让 Codex 去找一个不存在的环境变量
    if let Some(old) = router.remove("env_key") {
        changes.push(FieldChange { field: format!("model_providers.{PROVIDER_ID}.env_key"), before: old.as_str().map(str::to_string), after: None });
    }

    let content = doc.to_string();
    let unchanged = existing == Some(content.as_str());
    Ok(Plan { content, changes, unchanged })
}
