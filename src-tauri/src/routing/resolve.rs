//! 模型名解析（PRODUCT_DESIGN §4.2：模型名称冲突）。
//!
//! 解析顺序（确定性，任何一步命中即停止，不会跨步骤静默覆盖）：
//! 1. `public_alias` 精确匹配（别名是用户侧发布名，如 `ag/model-a`，保存时已保证已启用模型间唯一）。
//! 2. `供应商/模型名`：前缀是提供商 id 或目录别名（如 `ag`），后半是上游模型 ID 或该模型的别名。
//! 3. 裸上游模型 ID（如 `gpt-4o`）。多个提供商都提供时是**二义的**：返回 400 并列出候选，绝不选一个。
//!
//! 每一步先只看「适配器就绪」的提供商；全部没有就绪的才报告「适配器未接入」（501），避免把请求发给不存在的上游。
//! 密钥的 `allowed_models` 在每一步内先过滤候选，因此只放行某一个候选的密钥不会被二义性挡住。

use std::collections::HashMap;
use std::sync::OnceLock;

use super::repo::ModelCfg;

/// 一个可被客户端寻址的模型（已带上所属提供商的就绪状态）。
#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub model: ModelCfg,
    pub provider_alias: Option<String>,
    /// `Err(原因)` 表示适配器未就绪。
    pub ready: Result<(), String>,
}

impl ModelEntry {
    /// 对外展示名：优先别名，缺失时用 `提供商/上游模型`。
    pub fn public_name(&self) -> String {
        if self.model.public_alias.trim().is_empty() {
            self.qualified_name()
        } else {
            self.model.public_alias.clone()
        }
    }

    pub fn qualified_name(&self) -> String {
        format!("{}/{}", self.model.provider_id, self.model.upstream_model_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    NotFound,
    Ambiguous { candidates: Vec<String> },
    NotAllowed,
    /// 唯一命中的提供商没有就绪的适配器。
    NotReady { provider_id: String, reason: String },
}

/// 密钥 `allowed_models` 是否放行该模型。空列表 = 不限制。
/// 条目可以是：对外别名、上游模型 ID、`提供商/上游模型 ID`、内部 ID，或以 `*` 结尾的前缀通配。
pub fn is_allowed(patterns: &[String], entry: &ModelEntry) -> bool {
    if patterns.is_empty() {
        return true;
    }
    let names = [
        entry.model.public_alias.as_str(),
        entry.model.upstream_model_id.as_str(),
        entry.model.id.as_str(),
    ];
    let qualified = entry.qualified_name();
    patterns.iter().any(|p| {
        let p = p.trim();
        if p.is_empty() {
            return false;
        }
        if let Some(prefix) = p.strip_suffix('*') {
            return names.iter().any(|n| n.starts_with(prefix)) || qualified.starts_with(prefix);
        }
        names.contains(&p) || qualified == p
    })
}

fn matches_stage(stage: u8, requested: &str, e: &ModelEntry) -> bool {
    match stage {
        0 => !e.model.public_alias.is_empty() && e.model.public_alias == requested,
        1 => {
            let Some((prefix, rest)) = requested.split_once('/') else { return false };
            let provider_ok = e.model.provider_id == prefix || e.provider_alias.as_deref() == Some(prefix);
            provider_ok && (e.model.upstream_model_id == rest || e.model.public_alias == rest)
        }
        _ => e.model.upstream_model_id == requested,
    }
}

/// 返回命中条目在 `entries` 中的下标。
pub fn resolve(requested: &str, entries: &[ModelEntry], allowed: &[String]) -> Result<usize, ResolveError> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Err(ResolveError::NotFound);
    }
    let mut not_ready: Option<&ModelEntry> = None;
    let mut blocked = false;
    for stage in 0..3u8 {
        let all: Vec<usize> = (0..entries.len()).filter(|i| matches_stage(stage, requested, &entries[*i])).collect();
        if all.is_empty() {
            continue;
        }
        let ready: Vec<usize> = all.iter().copied().filter(|i| entries[*i].ready.is_ok()).collect();
        if ready.is_empty() {
            not_ready = not_ready.or(Some(&entries[all[0]]));
            continue;
        }
        let permitted: Vec<usize> = ready.iter().copied().filter(|i| is_allowed(allowed, &entries[*i])).collect();
        match permitted.len() {
            0 => {
                blocked = true;
                continue;
            }
            1 => return Ok(permitted[0]),
            _ => {
                let mut candidates: Vec<String> = permitted.iter().map(|i| entries[*i].public_name()).collect();
                // 别名也相同（理论上保存时已阻止）：退回完整限定名，保证候选可区分
                if has_duplicates(&candidates) {
                    candidates = permitted.iter().map(|i| entries[*i].qualified_name()).collect();
                }
                candidates.sort();
                candidates.dedup();
                return Err(ResolveError::Ambiguous { candidates });
            }
        }
    }
    if blocked {
        return Err(ResolveError::NotAllowed);
    }
    if let Some(e) = not_ready {
        return Err(ResolveError::NotReady {
            provider_id: e.model.provider_id.clone(),
            reason: e.ready.clone().err().unwrap_or_default(),
        });
    }
    Err(ResolveError::NotFound)
}

fn has_duplicates(v: &[String]) -> bool {
    let mut s = v.to_vec();
    s.sort();
    s.windows(2).any(|w| w[0] == w[1])
}

/// 内置目录里的提供商别名（`provider_catalog.json` 的 `alias`），用于 `供应商别名/模型名`。
pub fn catalog_alias(provider_id: &str) -> Option<String> {
    static ALIASES: OnceLock<HashMap<String, String>> = OnceLock::new();
    ALIASES
        .get_or_init(|| {
            let raw = include_str!("../../resources/provider_catalog.json");
            let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(raw) else {
                return HashMap::new();
            };
            items
                .iter()
                .filter_map(|i| Some((i.get("id")?.as_str()?.to_string(), i.get("alias")?.as_str()?.to_string())))
                .filter(|(id, alias)| id != alias)
                .collect()
        })
        .get(provider_id)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(provider: &str, upstream: &str, alias: &str, ready: bool) -> ModelEntry {
        ModelEntry {
            model: ModelCfg {
                id: format!("{provider}/{upstream}"),
                provider_id: provider.into(),
                upstream_model_id: upstream.into(),
                public_alias: alias.into(),
            },
            provider_alias: catalog_alias(provider),
            ready: if ready { Ok(()) } else { Err("not wired".into()) },
        }
    }

    fn sample() -> Vec<ModelEntry> {
        vec![
            entry("antigravity", "gpt-4o", "ag/gpt-4o", true),
            entry("openrouter", "gpt-4o", "or/gpt-4o", true),
            entry("openrouter", "llama-3", "llama", true),
            entry("kimi", "k2", "kimi/k2", false),
        ]
    }

    #[test]
    fn alias_wins_and_is_exact() {
        let e = sample();
        assert_eq!(resolve("ag/gpt-4o", &e, &[]), Ok(0));
        assert_eq!(resolve("llama", &e, &[]), Ok(2));
        assert_eq!(resolve("  or/gpt-4o ", &e, &[]), Ok(1));
        assert_eq!(resolve("AG/GPT-4O", &e, &[]), Err(ResolveError::NotFound));
        assert_eq!(resolve("", &e, &[]), Err(ResolveError::NotFound));
    }

    #[test]
    fn provider_qualified_with_id_or_catalog_alias() {
        let e = sample();
        assert_eq!(catalog_alias("antigravity").as_deref(), Some("ag"));
        assert_eq!(resolve("antigravity/gpt-4o", &e, &[]), Ok(0));
        assert_eq!(resolve("openrouter/llama-3", &e, &[]), Ok(2));
        // 前缀 `ag` 同时是别名，但按提供商别名也可寻址上游 ID
        assert_eq!(resolve("ag/gpt-4o", &e, &[]), Ok(0));
        assert_eq!(resolve("nope/gpt-4o", &e, &[]), Err(ResolveError::NotFound));
    }

    #[test]
    fn bare_upstream_id_is_ambiguous_across_providers() {
        let e = sample();
        let err = resolve("gpt-4o", &e, &[]).unwrap_err();
        assert_eq!(err, ResolveError::Ambiguous { candidates: vec!["ag/gpt-4o".into(), "or/gpt-4o".into()] });
        // 唯一时可用裸 ID
        assert_eq!(resolve("llama-3", &e, &[]), Ok(2));
    }

    #[test]
    fn allowed_models_filter_candidates_before_ambiguity_check() {
        let e = sample();
        assert_eq!(resolve("gpt-4o", &e, &["or/gpt-4o".into()]), Ok(1));
        assert_eq!(resolve("ag/gpt-4o", &e, &["or/gpt-4o".into()]), Err(ResolveError::NotAllowed));
        assert_eq!(resolve("ag/gpt-4o", &e, &["ag/*".into()]), Ok(0));
        assert_eq!(resolve("llama", &e, &["llama-3".into()]), Ok(2), "上游 ID 也能放行别名");
        assert_eq!(resolve("nonexistent", &e, &["x".into()]), Err(ResolveError::NotFound));
    }

    #[test]
    fn not_ready_provider_is_reported_not_hidden() {
        let e = sample();
        assert_eq!(
            resolve("kimi/k2", &e, &[]),
            Err(ResolveError::NotReady { provider_id: "kimi".into(), reason: "not wired".into() })
        );
        // 就绪的候选存在时，不就绪的不参与
        let mut e2 = sample();
        e2.push(entry("kimi", "gpt-4o", "kimi/gpt-4o", false));
        assert_eq!(resolve("ag/gpt-4o", &e2, &[]), Ok(0));
    }

    #[test]
    fn duplicate_alias_yields_qualified_candidates() {
        let e = vec![entry("a", "x", "same", true), entry("b", "y", "same", true)];
        let err = resolve("same", &e, &[]).unwrap_err();
        assert_eq!(err, ResolveError::Ambiguous { candidates: vec!["a/x".into(), "b/y".into()] });
    }
}
