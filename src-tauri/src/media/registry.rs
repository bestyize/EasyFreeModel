//! 媒体供应商注册表：数据来自 `resources/media_providers.json`（编译进二进制）。
//!
//! 每家供应商、每种能力一条配置：格式标识、baseUrl、鉴权方式、默认模型、可选参数能力表、
//! 状态（ready | deferred）。适配器按格式标识复用；未接入的条目显式标 deferred 并写明原因，
//! 界面据此显示「待接入」，绝不伪造可用状态。

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::models::MediaKind;

pub const MEDIA_PROVIDERS_JSON: &str = include_str!("../../resources/media_providers.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    Image,
    Stt,
    Tts,
    Video,
    Search,
    /// 网页抓取（Firecrawl / Jina Reader 等），经 `submit_media_job(kind=search, params.url)` 调用。
    Fetch,
    /// 文本嵌入：不是 MediaKind，供网关/内部直接调用 `embedding::embed`。
    Embedding,
}

impl Capability {
    pub const ALL: [Capability; 7] = [
        Capability::Image,
        Capability::Stt,
        Capability::Tts,
        Capability::Video,
        Capability::Search,
        Capability::Fetch,
        Capability::Embedding,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Image => "image",
            Capability::Stt => "stt",
            Capability::Tts => "tts",
            Capability::Video => "video",
            Capability::Search => "search",
            Capability::Fetch => "fetch",
            Capability::Embedding => "embedding",
        }
    }

    pub fn for_kind(kind: MediaKind) -> Capability {
        match kind {
            MediaKind::Image => Capability::Image,
            MediaKind::Asr => Capability::Stt,
            MediaKind::Tts => Capability::Tts,
            MediaKind::Video => Capability::Video,
            MediaKind::Search => Capability::Search,
        }
    }

    /// 该能力对应的任务类型（嵌入没有）。
    pub fn kind(self) -> Option<MediaKind> {
        match self {
            Capability::Image => Some(MediaKind::Image),
            Capability::Stt => Some(MediaKind::Asr),
            Capability::Tts => Some(MediaKind::Tts),
            Capability::Video => Some(MediaKind::Video),
            Capability::Search | Capability::Fetch => Some(MediaKind::Search),
            Capability::Embedding => None,
        }
    }

    /// 默认轮询间隔（毫秒）与总超时（秒）；JSON 可逐条覆盖。
    pub fn default_polling(self) -> (u64, u64) {
        match self {
            Capability::Video => (5_000, 1_800),
            Capability::Image => (2_000, 300),
            _ => (2_000, 600),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Ready,
    Deferred,
}

/// 鉴权方式：bearer | header | query | none。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AuthSpec {
    #[default]
    Bearer,
    /// 自定义请求头，`prefix` 形如 `"Token "` / `"Key "`。
    Header {
        name: String,
        #[serde(default)]
        prefix: String,
    },
    /// 放进查询参数（会进 URL，错误信息里一律剥掉 URL）。
    Query { name: String },
    None,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CapabilityConfig {
    pub status: Status,
    /// deferred 时的原因。
    pub reason: Option<String>,
    pub format: String,
    pub base_url: String,
    pub auth: AuthSpec,
    pub default_model: Option<String>,
    /// 建议模型列表（界面提示用，不做强制）。
    pub models: Vec<String>,
    /// 可选参数白名单：不在其中的可选字段不会发给上游。
    pub params: Vec<String>,
    /// 特性标记（如 `asr-timestamps`、`image-edit`），前端据此启用/禁用控件。
    pub features: Vec<String>,
    /// 固定的非秘密请求头（版本头等）。
    pub extra_headers: BTreeMap<String, String>,
    /// 额外端点（如 `edit`）。
    pub endpoints: BTreeMap<String, String>,
    /// 除 baseUrl 域名外，允许携带凭据访问的后续地址域名后缀（如 `bfl.ai`）。
    pub trusted_hosts: Vec<String>,
    pub poll_interval_ms: Option<u64>,
    pub poll_timeout_secs: Option<u64>,
}

impl CapabilityConfig {
    pub fn is_ready(&self) -> bool {
        self.status == Status::Ready
    }

    pub fn has_param(&self, name: &str) -> bool {
        self.params.iter().any(|p| p == name)
    }

    pub fn has_feature(&self, name: &str) -> bool {
        self.features.iter().any(|p| p == name)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProviderEntry {
    pub id: String,
    pub name: String,
    /// 整家供应商 deferred（如依赖私有网页接口的 Cookie 类）时填写；否则由各能力状态推导。
    pub status: Option<Status>,
    pub reason: Option<String>,
    pub capabilities: BTreeMap<Capability, CapabilityConfig>,
}

impl ProviderEntry {
    /// 至少一种能力可用才算 ready。
    pub fn is_ready(&self) -> bool {
        self.status != Some(Status::Deferred) && self.capabilities.values().any(CapabilityConfig::is_ready)
    }

    pub fn ready_capabilities(&self) -> impl Iterator<Item = (&Capability, &CapabilityConfig)> {
        let enabled = self.status != Some(Status::Deferred);
        self.capabilities.iter().filter(move |(_, c)| enabled && c.is_ready())
    }
}

#[derive(Debug, Deserialize)]
struct MediaProvidersFile {
    #[allow(dead_code)]
    version: u32,
    providers: Vec<ProviderEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct MediaRegistry {
    providers: Vec<ProviderEntry>,
    index: HashMap<String, usize>,
}

static BUILTIN: OnceLock<MediaRegistry> = OnceLock::new();

impl MediaRegistry {
    pub fn from_json(json: &str) -> Result<Self, String> {
        let file: MediaProvidersFile = serde_json::from_str(json).map_err(|e| format!("media_providers.json 解析失败：{e}"))?;
        let mut index = HashMap::new();
        for (i, p) in file.providers.iter().enumerate() {
            if p.id.trim().is_empty() {
                return Err("media_providers.json：供应商 id 不能为空".into());
            }
            if index.insert(p.id.clone(), i).is_some() {
                return Err(format!("media_providers.json：供应商 id 重复：{}", p.id));
            }
            for (cap, c) in &p.capabilities {
                match c.status {
                    Status::Ready => {
                        if c.format.is_empty() || c.base_url.is_empty() {
                            return Err(format!("{} / {}：ready 的能力必须有 format 与 baseUrl", p.id, cap.as_str()));
                        }
                    }
                    Status::Deferred => {
                        if c.reason.as_deref().unwrap_or("").is_empty() {
                            return Err(format!("{} / {}：deferred 必须写明原因", p.id, cap.as_str()));
                        }
                    }
                }
            }
            if p.status == Some(Status::Deferred) && p.reason.as_deref().unwrap_or("").is_empty() {
                return Err(format!("{}：deferred 必须写明原因", p.id));
            }
        }
        Ok(MediaRegistry { providers: file.providers, index })
    }

    /// 内置注册表。解析失败（只可能是打包错误，测试会拦截）时退化为空表，所有供应商显示「待接入」。
    pub fn builtin() -> &'static MediaRegistry {
        BUILTIN.get_or_init(|| match MediaRegistry::from_json(MEDIA_PROVIDERS_JSON) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("{e}");
                MediaRegistry::default()
            }
        })
    }

    pub fn get(&self, id: &str) -> Option<&ProviderEntry> {
        self.index.get(id).map(|i| &self.providers[*i])
    }

    pub fn providers(&self) -> &[ProviderEntry] {
        &self.providers
    }

    /// 取某供应商某能力的配置（含 deferred，调用方自行判断状态）。
    pub fn capability(&self, id: &str, cap: Capability) -> Option<&CapabilityConfig> {
        self.get(id).and_then(|p| p.capabilities.get(&cap))
    }

    /// 可用于提交任务的配置：供应商未整体 deferred 且该能力 ready。
    pub fn ready_capability(&self, id: &str, cap: Capability) -> Option<&CapabilityConfig> {
        let p = self.get(id)?;
        if p.status == Some(Status::Deferred) {
            return None;
        }
        p.capabilities.get(&cap).filter(|c| c.is_ready())
    }

    pub fn ready_provider_ids(&self, cap: Capability) -> Vec<&str> {
        self.providers.iter().filter(|p| self.ready_capability(&p.id, cap).is_some()).map(|p| p.id.as_str()).collect()
    }

    /// 任意能力 ready 的供应商 id（用于把 providers 表的 adapter_ready 置位）。
    pub fn all_ready_provider_ids(&self) -> Vec<&str> {
        self.providers.iter().filter(|p| p.is_ready()).map(|p| p.id.as_str()).collect()
    }

    /// 测试用：把某能力的 baseUrl（及端点、受信域名）改指向假上游。
    #[cfg(test)]
    pub fn with_base_url(mut self, id: &str, cap: Capability, base_url: &str) -> Self {
        if let Some(i) = self.index.get(id).copied() {
            if let Some(c) = self.providers[i].capabilities.get_mut(&cap) {
                c.base_url = base_url.to_string();
                for v in c.endpoints.values_mut() {
                    *v = format!("{base_url}/__endpoint");
                }
            }
        }
        self
    }

    #[cfg(test)]
    pub fn with_capability(mut self, id: &str, cap: Capability, cfg: CapabilityConfig) -> Self {
        let i = match self.index.get(id).copied() {
            Some(i) => i,
            None => {
                self.providers.push(ProviderEntry { id: id.to_string(), name: id.to_string(), ..Default::default() });
                let i = self.providers.len() - 1;
                self.index.insert(id.to_string(), i);
                i
            }
        };
        self.providers[i].capabilities.insert(cap, cfg);
        self
    }
}

// ---- 给前端/命令层的只读视图 ----

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityInfo {
    pub capability: Capability,
    pub status: Status,
    pub reason: Option<String>,
    pub format: String,
    pub default_model: Option<String>,
    pub models: Vec<String>,
    pub params: Vec<String>,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaProviderInfo {
    pub id: String,
    pub name: String,
    pub status: Status,
    pub reason: Option<String>,
    pub capabilities: Vec<CapabilityInfo>,
}

impl MediaRegistry {
    /// 列出供应商视图；`capability` 非空时只返回具备该能力（含 deferred）的条目。
    pub fn infos(&self, capability: Option<Capability>) -> Vec<MediaProviderInfo> {
        self.providers
            .iter()
            .filter(|p| capability.is_none_or(|c| p.capabilities.contains_key(&c)))
            .map(|p| MediaProviderInfo {
                id: p.id.clone(),
                name: p.name.clone(),
                status: if p.is_ready() { Status::Ready } else { Status::Deferred },
                reason: p.reason.clone().or_else(|| {
                    if p.is_ready() {
                        None
                    } else {
                        p.capabilities.values().find_map(|c| c.reason.clone())
                    }
                }),
                capabilities: p
                    .capabilities
                    .iter()
                    .filter(|(cap, _)| capability.is_none_or(|c| **cap == c))
                    .map(|(cap, c)| CapabilityInfo {
                        capability: *cap,
                        status: if p.status == Some(Status::Deferred) { Status::Deferred } else { c.status },
                        reason: c.reason.clone(),
                        format: c.format.clone(),
                        default_model: c.default_model.clone(),
                        models: c.models.clone(),
                        params: c.params.clone(),
                        features: c.features.clone(),
                    })
                    .collect(),
            })
            .collect()
    }
}
