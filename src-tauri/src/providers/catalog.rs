//! 内置提供商目录（PRODUCT_DESIGN.md §4.2）：133 家，对齐 9Router。
//!
//! 数据来源是内嵌的两份 JSON：
//! - `resources/provider_catalog.json`：名称 / 分类 / 颜色 / 官网 / 服务类型 / 认证方式（只读，不含任何凭据）；
//! - `resources/provider_status.json`：适配器是否真实可用的单一来源（见 `status.rs`）。
//!
//! 启动时 `seed` 幂等写入全部条目：新条目入库，已存在条目只刷新目录字段，不触碰用户数据。

use std::sync::OnceLock;

use serde::Deserialize;
use sqlx::SqlitePool;

use crate::error::AppResult;
use crate::models::{CredentialKind, Protocol, ProviderGroup};
use crate::providers::status::{self, StatusEntry};
use crate::store::{self, providers as repo};

pub const CATALOG_JSON: &str = include_str!("../../resources/provider_catalog.json");

pub const OAUTH_RISK_NOTICE: &str = "该接入方式依赖 OAuth/订阅账户，可能受上游服务条款限制。\
仅在你本人授权且上游允许的前提下使用，不要用于规避服务限制。";

/// 目录中的一项（字段与 provider_catalog.json 一一对应）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub id: String,
    pub alias: String,
    pub name: String,
    /// oauth | apikey | freeTier | free | webCookie
    pub category: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub text_icon: Option<String>,
    #[serde(default)]
    pub website: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub base_url_host: Option<String>,
    /// 注意目录里的键名是 `hasOAuth`（不是 camelCase 的 hasOauth）
    #[serde(default, rename = "hasOAuth")]
    pub has_oauth: bool,
    #[serde(default)]
    pub oauth_flow: Option<String>,
    #[serde(default)]
    pub auth_modes: Vec<String>,
    #[serde(default)]
    pub no_auth: bool,
    #[serde(default)]
    pub service_kinds: Vec<String>,
    #[serde(default)]
    pub model_count: i64,
    #[serde(default)]
    pub deprecated: bool,
    /// 上游目录里只是一个占位标记（值为常量名，不是文案）；仅用来判断"该条目需要展示风险提示"。
    #[serde(default)]
    pub risk_notice: Option<String>,
}

impl CatalogEntry {
    /// oauth → oauth；free/freeTier → free；apikey 与 webCookie → apikey。
    pub fn group(&self) -> ProviderGroup {
        match self.category.as_str() {
            "oauth" => ProviderGroup::Oauth,
            "free" | "freeTier" => ProviderGroup::Free,
            _ => ProviderGroup::Apikey,
        }
    }

    /// 目录里没有声明服务类型的（azure / chutes / bai / gitlab）按 9Router 的默认视为 llm。
    pub fn service_kinds(&self) -> Vec<String> {
        if self.service_kinds.is_empty() {
            vec!["llm".to_string()]
        } else {
            self.service_kinds.clone()
        }
    }

    /// 目录里写作 "apikey" / "oauth"，对外统一为 CredentialKind（"apiKey" / "oauth"）。
    pub fn auth_modes(&self) -> Vec<CredentialKind> {
        let mut out: Vec<CredentialKind> = Vec::new();
        for m in &self.auth_modes {
            let k = match m.to_ascii_lowercase().as_str() {
                "oauth" => CredentialKind::Oauth,
                "apikey" => CredentialKind::ApiKey,
                _ => continue,
            };
            if !out.contains(&k) {
                out.push(k);
            }
        }
        if out.is_empty() {
            out.push(CredentialKind::ApiKey);
        }
        out
    }

    /// 只给出上游声明的原生协议，不猜测；未知留空。
    pub fn protocols(&self) -> Vec<Protocol> {
        match self.format.as_deref() {
            Some("openai") => vec![Protocol::OpenaiChat],
            Some("claude") => vec![Protocol::AnthropicMessages],
            Some("openai-responses") => vec![Protocol::OpenaiResponses],
            Some("gemini") | Some("vertex") => vec![Protocol::Gemini],
            _ => vec![],
        }
    }

    /// 仅 OAuth/订阅类展示风险提示：category=oauth，或目录标记了风险（如 Gemini CLI、Kiro）。
    pub fn risk_notice_text(&self) -> Option<&'static str> {
        (self.category == "oauth" || (self.has_oauth && self.risk_notice.is_some())).then_some(OAUTH_RISK_NOTICE)
    }

    /// 规范化为 `#RRGGBB`：目录里有 `#ffffffff` 之类 8 位写法，取前 6 位；无效则为 None。
    pub fn color(&self) -> Option<String> {
        normalize_color(self.color.as_deref())
    }
}

pub fn normalize_color(raw: Option<&str>) -> Option<String> {
    let s = raw?.trim().strip_prefix('#')?;
    if !(s.len() == 6 || s.len() == 8) || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("#{}", s[..6].to_ascii_uppercase()))
}

fn parse_catalog() -> Result<Vec<CatalogEntry>, String> {
    let list: Vec<CatalogEntry> =
        serde_json::from_str(CATALOG_JSON).map_err(|e| format!("provider_catalog.json 解析失败：{e}"))?;
    let mut seen = std::collections::HashSet::new();
    for e in &list {
        if e.id.is_empty() || e.name.is_empty() || !seen.insert(e.id.clone()) {
            return Err(format!("provider_catalog.json 中 id 为空或重复：{}", e.id));
        }
    }
    Ok(list)
}

/// 顺序即界面展示顺序（sort_order 取下标）。
pub fn entries() -> &'static [CatalogEntry] {
    static CATALOG: OnceLock<Vec<CatalogEntry>> = OnceLock::new();
    CATALOG.get_or_init(|| parse_catalog().expect("内嵌的 provider_catalog.json 无效"))
}

pub fn find(id: &str) -> Option<&'static CatalogEntry> {
    entries().iter().find(|c| c.id == id)
}

/// adapter 为 null 的条目必有 reason（status.rs 在解析时已校验）；缺失目录项时给兜底文案。
fn status_of(id: &str) -> Option<&'static StatusEntry> {
    status::get(id)
}

/// 幂等写入目录：新增条目入库；已存在条目只刷新目录字段，不触碰用户数据
/// （连接、模型、启用状态、分发策略、请求头）。整个过程在一个事务里。
pub async fn seed(pool: &SqlitePool) -> AppResult<()> {
    let now = store::now_ts();
    let media_ready: std::collections::HashSet<&str> = crate::media::ready_provider_ids().into_iter().collect();
    let mut tx = pool.begin().await?;
    for (idx, entry) in entries().iter().enumerate() {
        let st = status_of(&entry.id);
        let adapter = st.and_then(|s| s.adapter);
        // 聊天适配器或媒体/搜索/嵌入适配器任一就绪即可录入凭据；聊天路由仍只认 provider_status.json
        let ready = adapter.is_some() || media_ready.contains(entry.id.as_str());
        let pending = if ready {
            None
        } else {
            Some(st.and_then(|s| s.reason.as_deref()).unwrap_or("该提供商尚未登记适配器状态，暂不可用"))
        };
        let protocols: Vec<&str> = entry.protocols().iter().map(|p| p.as_str()).collect();
        let auth_modes: Vec<&str> = entry.auth_modes().iter().map(|k| k.as_str()).collect();
        let color = entry.color();
        let write = repo::CatalogWrite {
            id: &entry.id,
            name: &entry.name,
            grp: entry.group().as_str(),
            adapter: adapter.map(|a| a.as_str()).unwrap_or("none"),
            adapter_ready: ready,
            protocols_json: serde_json::to_string(&protocols)?,
            risk_notice: entry.risk_notice_text(),
            sort_order: idx as i64,
            category: &entry.category,
            service_kinds_json: serde_json::to_string(&entry.service_kinds())?,
            website: entry.website.as_deref(),
            color: color.as_deref(),
            text_icon: entry.text_icon.as_deref(),
            auth_modes_json: serde_json::to_string(&auth_modes)?,
            pending_reason: pending,
            base_url: st.and_then(|s| s.base_url.as_deref()),
        };
        if !repo::upsert_catalog_entry(&mut tx, &write, &now).await? {
            tracing::warn!("内置提供商 id 与已有的自定义提供商冲突，已跳过：{}", entry.id);
        }
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn catalog_parses_and_has_133_unique_ids() {
        let list = entries();
        assert_eq!(list.len(), 133);
        let ids: HashSet<_> = list.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids.len(), 133);
        // 旧版 33 家依赖的几个 id 仍在
        for id in ["openrouter", "gemini", "nvidia", "claude", "antigravity"] {
            assert!(find(id).is_some(), "{id}");
        }
    }

    #[test]
    fn status_file_covers_exactly_the_catalog() {
        let cat: HashSet<_> = entries().iter().map(|c| c.id.as_str()).collect();
        let st: HashSet<_> = status::all().keys().map(|k| k.as_str()).collect();
        assert_eq!(cat, st, "provider_status.json 必须与目录 id 一一对应");
    }

    #[test]
    fn group_mapping_follows_category() {
        let mut counts: HashMap<ProviderGroup, usize> = HashMap::new();
        for e in entries() {
            *counts.entry(e.group()).or_default() += 1;
            match e.category.as_str() {
                "oauth" => assert_eq!(e.group(), ProviderGroup::Oauth),
                "free" | "freeTier" => assert_eq!(e.group(), ProviderGroup::Free),
                "apikey" | "webCookie" => assert_eq!(e.group(), ProviderGroup::Apikey),
                other => panic!("未知 category {other}"),
            }
        }
        assert_eq!(counts[&ProviderGroup::Oauth], 23);
        assert_eq!(counts[&ProviderGroup::Free], 23);
        assert_eq!(counts[&ProviderGroup::Apikey], 87);
    }

    #[test]
    fn status_rules_hold_for_every_entry() {
        for e in entries() {
            let st = status::get(&e.id).unwrap();
            if e.category == "webCookie" {
                assert!(st.adapter.is_none(), "{} 属于网页会话类，不得标记就绪", e.id);
            }
            if !e.service_kinds().iter().any(|k| k == "llm") {
                assert!(st.adapter.is_none(), "{} 不是聊天类，不得标记聊天适配器", e.id);
            }
            if st.adapter.is_none() {
                assert!(!st.reason.as_deref().unwrap_or("").trim().is_empty(), "{} 缺少 reason", e.id);
            }
        }
        // 抽样：规则里明确点名的几类
        use status::Adapter::*;
        assert_eq!(status::get("openai").unwrap().adapter, Some(OpenaiCompatible));
        assert_eq!(status::get("anthropic").unwrap().adapter, Some(AnthropicCompatible));
        assert_eq!(status::get("gemini").unwrap().adapter, Some(Gemini));
        assert_eq!(status::get("vertex").unwrap().adapter, Some(Vertex));
        assert_eq!(status::get("ollama").unwrap().adapter, Some(Ollama));
        assert_eq!(status::get("claude").unwrap().adapter, Some(AnthropicCompatible));
        assert_eq!(status::get("codex").unwrap().adapter, Some(OpenaiResponses));
        assert_eq!(status::get("xai").unwrap().adapter, Some(OpenaiCompatible));
        assert!(status::get("grok-web").unwrap().adapter.is_none());
        assert!(status::get("elevenlabs").unwrap().adapter.is_none());
    }

    #[test]
    fn risk_notice_only_for_oauth_like_and_never_the_raw_placeholder() {
        for e in entries() {
            if let Some(t) = e.risk_notice_text() {
                assert_ne!(t, "RISK_NOTICE");
                assert!(e.category == "oauth" || e.has_oauth, "{} 不应展示风险提示", e.id);
            }
        }
        assert!(find("openrouter").unwrap().risk_notice_text().is_none());
        assert!(find("claude").unwrap().risk_notice_text().is_some());
        assert!(find("gemini-cli").unwrap().risk_notice_text().is_some());
    }

    #[test]
    fn has_oauth_flag_is_read_from_the_catalog() {
        let n = entries().iter().filter(|e| e.has_oauth).count();
        assert_eq!(n, 25, "目录中 hasOAuth=true 的条目数");
        assert!(find("gemini-cli").unwrap().has_oauth);
        assert!(!find("openai").unwrap().has_oauth);
    }

    #[test]
    fn colors_are_normalized() {
        assert_eq!(normalize_color(Some("#ffffffff")).as_deref(), Some("#FFFFFF"));
        assert_eq!(normalize_color(Some("#ff6a00")).as_deref(), Some("#FF6A00"));
        assert_eq!(normalize_color(Some("red")), None);
        assert_eq!(normalize_color(Some("#12")), None);
        assert_eq!(normalize_color(None), None);
    }

    #[test]
    fn defaults_for_missing_service_kinds_and_auth_modes() {
        for id in ["azure", "chutes", "bai", "gitlab"] {
            assert_eq!(find(id).unwrap().service_kinds(), vec!["llm".to_string()], "{id}");
        }
        assert_eq!(find("clinepass").unwrap().auth_modes(), vec![CredentialKind::ApiKey, CredentialKind::Oauth]);
        assert_eq!(find("openai").unwrap().auth_modes(), vec![CredentialKind::ApiKey]);
    }
}
