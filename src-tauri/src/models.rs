//! 与前端 `src/types/index.ts` 一一对应的数据结构。
//! 字段 camelCase，枚举取值与 TS 字符串字面量一致。

use serde::{Deserialize, Serialize};

use crate::gateway::GatewayStatus;

/// 生成带 serde 取值、`as_str`、`parse` 的字符串枚举，便于与 SQLite TEXT 列互转。
macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $s)] $variant),+
        }

        impl $name {
            pub fn as_str(&self) -> &'static str {
                match self { $(Self::$variant => $s),+ }
            }

            pub fn parse(s: &str) -> Option<Self> {
                match s { $($s => Some(Self::$variant),)+ _ => None }
            }

            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
        }
    };
}

string_enum!(ProviderGroup { Custom => "custom", Oauth => "oauth", Free => "free", Apikey => "apikey" });
string_enum!(ConnectionStatus {
    Connected => "connected",
    Disconnected => "disconnected",
    Error => "error",
    Unsupported => "unsupported",
    Checking => "checking",
});
string_enum!(CredentialKind { Oauth => "oauth", ApiKey => "apiKey" });
string_enum!(Protocol {
    OpenaiChat => "openai-chat",
    OpenaiResponses => "openai-responses",
    AnthropicMessages => "anthropic-messages",
    Gemini => "gemini",
});
string_enum!(AccountStatus {
    Active => "active",
    Paused => "paused",
    Error => "error",
    // OAuth 令牌已失效（refresh token 被撤销 / 过期，或没有可用令牌）：需要用户重新授权，不参与路由
    NeedsReauth => "needsReauth",
});
string_enum!(ThinkingLevel { Auto => "auto", Low => "low", Medium => "medium", High => "high" });
string_enum!(UsageRange {
    Today => "today",
    Hours24 => "24h",
    Days7 => "7d",
    Days30 => "30d",
    All => "all",
});
string_enum!(UsageStatus { Success => "success", Error => "error", Cancelled => "cancelled" });
string_enum!(TokenSource { Upstream => "upstream", Estimated => "estimated" });
string_enum!(QuotaSource {
    Live => "live",
    Cached => "cached",
    Estimated => "estimated",
    Unknown => "unknown",
});
string_enum!(Language { ZhCn => "zh-CN", En => "en" });
string_enum!(Theme { Light => "light", Dark => "dark", System => "system" });
string_enum!(MediaKind {
    Image => "image",
    Asr => "asr",
    Tts => "tts",
    Video => "video",
    Search => "search",
});
string_enum!(JobStatus {
    Queued => "queued",
    Running => "running",
    Succeeded => "succeeded",
    Failed => "failed",
    Cancelled => "cancelled",
});

// ---- 提供商 ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSummary {
    pub id: String,
    pub name: String,
    pub group: ProviderGroup,
    pub status: ConnectionStatus,
    pub connection_count: u32,
    pub adapter_ready: bool,
    pub protocols: Vec<Protocol>,
    pub quota_hint: Option<String>,
    /// 目录声明的服务类型（llm / image / tts / stt / video / webSearch / webFetch / embedding / imageToText / systemone）
    #[serde(default)]
    pub service_kinds: Vec<String>,
    #[serde(default)]
    pub website: Option<String>,
    /// `#RRGGBB`；目录里无效/缺失时为 null
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub text_icon: Option<String>,
    #[serde(default)]
    pub auth_modes: Vec<CredentialKind>,
    /// adapter_ready=false 时向用户展示的"待接入"原因；就绪时为 null
    #[serde(default)]
    pub pending_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: String,
    pub provider_id: String,
    pub account_label: String,
    pub kind: CredentialKind,
    pub status: AccountStatus,
    pub priority: i64,
    pub weight: i64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    pub id: String,
    pub upstream_model_id: String,
    pub public_alias: String,
    pub description: String,
    pub enabled: bool,
    pub supports_thinking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DistributionPolicy {
    pub round_robin: bool,
    pub max_concurrency: i64,
    pub max_retries: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomHeader {
    pub key: String,
    pub value: String,
    pub secret: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDetail {
    #[serde(flatten)]
    pub summary: ProviderSummary,
    pub risk_notice: Option<String>,
    pub connections: Vec<Connection>,
    pub distribution: DistributionPolicy,
    pub custom_headers: Vec<CustomHeader>,
    pub models: Vec<ModelEntry>,
    pub thinking_level: ThinkingLevel,
    /// 上游地址（非机密）：自定义提供商为用户填写的 Base URL，内置为 provider_status.json 的端点；无则 null
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    /// 提供商未就绪或没有连接时为空串
    pub connection_id: String,
    /// 结果所属的提供商（批量测试按它归组）
    #[serde(default)]
    pub provider_id: Option<String>,
    pub ok: bool,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
}

// ---- API 密钥 ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyInfo {
    pub id: String,
    pub name: String,
    pub masked: String,
    #[serde(default)]
    pub secret: Option<String>,
    pub protocols: Vec<Protocol>,
    pub allowed_models: Vec<String>,
    pub rpm_limit: Option<i64>,
    pub tpm_limit: Option<i64>,
    pub total_limit: Option<i64>,
    pub call_count: i64,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateApiKeyInput {
    pub name: String,
    pub expires_at: Option<String>,
    pub protocols: Vec<Protocol>,
    pub allowed_models: Vec<String>,
    pub rpm_limit: Option<i64>,
    pub tpm_limit: Option<i64>,
    pub total_limit: Option<i64>,
}

/// 完整密钥只在创建/轮换时返回这一次。不实现 Debug 的自动派生以外的日志输出，
/// 且该类型绝不应被 tracing 记录。
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeySecretResult {
    pub key: ApiKeyInfo,
    pub secret: String,
}

impl std::fmt::Debug for ApiKeySecretResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeySecretResult")
            .field("key", &self.key)
            .field("secret", &"<redacted>")
            .finish()
    }
}

// ---- 使用情况 ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetric {
    pub value: f64,
    pub delta_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrendPoint {
    pub label: String,
    pub input: i64,
    pub output: i64,
    pub cached: i64,
    pub requests: i64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderShare {
    pub provider_id: String,
    pub name: String,
    pub tokens: i64,
    pub pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRank {
    pub model: String,
    pub provider_id: String,
    pub tokens: i64,
    pub pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestShare {
    pub provider_id: String,
    pub name: String,
    pub requests: i64,
    pub pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub range: UsageRange,
    pub requests: UsageMetric,
    pub input_tokens: UsageMetric,
    pub output_tokens: UsageMetric,
    pub cached_tokens: UsageMetric,
    pub estimated_cost: UsageMetric,
    pub cost_source: String,
    pub trend: Vec<TrendPoint>,
    pub provider_share: Vec<ProviderShare>,
    pub model_ranking: Vec<ModelRank>,
    pub request_distribution: Vec<RequestShare>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRequest {
    pub request_id: String,
    pub provider_id: String,
    pub account_id: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub latency_ms: i64,
    pub status: UsageStatus,
    pub estimated_cost: f64,
    pub token_source: TokenSource,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageFilter {
    pub range: UsageRange,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub status: Option<UsageStatus>,
    #[serde(default)]
    pub protocol: Option<Protocol>,
    #[serde(default)]
    pub query: Option<String>,
}

// ---- 配额 ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaPeriod {
    pub name: String,
    pub unit: String,
    pub used: Option<f64>,
    pub total: Option<f64>,
    pub remaining: Option<f64>,
    pub resets_at: Option<String>,
    pub source: QuotaSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaBalance {
    pub currency: String,
    pub amount: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaAccount {
    pub connection_id: String,
    pub provider_id: String,
    pub provider_name: String,
    pub account_label: String,
    pub enabled: bool,
    pub periods: Vec<QuotaPeriod>,
    pub balances: Vec<QuotaBalance>,
    pub last_synced_at: Option<String>,
    pub error: Option<String>,
}

// ---- 设置与服务 ----

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemStatus {
    pub gateway: GatewayStatus,
    pub api_requests: i64,
    pub success_rate: Option<f64>,
    pub avg_latency_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub listen_address: String,
    pub port: u16,
    pub auto_start: bool,
    pub tray_enabled: bool,
    pub timeout_seconds: u32,
    pub retry_count: u32,
    pub language: Language,
    pub theme: Theme,
    pub font_scale: f64,
    pub redact_logs: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            listen_address: "127.0.0.1".into(),
            port: 20128,
            auto_start: false,
            tray_enabled: false,
            timeout_seconds: 120,
            retry_count: 2,
            language: Language::ZhCn,
            theme: Theme::Light,
            font_scale: 1.0,
            redact_logs: true,
        }
    }
}

// ---- 多模态作业 ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaJob {
    pub id: String,
    pub kind: MediaKind,
    pub provider_id: String,
    pub model: String,
    pub status: JobStatus,
    pub summary: String,
    pub output_path: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_wire_values_match_ts_literals() {
        assert_eq!(serde_json::to_string(&Protocol::OpenaiChat).unwrap(), "\"openai-chat\"");
        assert_eq!(serde_json::to_string(&ConnectionStatus::Connected).unwrap(), "\"connected\"");
        assert_eq!(serde_json::to_string(&CredentialKind::ApiKey).unwrap(), "\"apiKey\"");
        assert_eq!(serde_json::to_string(&UsageRange::Hours24).unwrap(), "\"24h\"");
        assert_eq!(serde_json::to_string(&Language::ZhCn).unwrap(), "\"zh-CN\"");
        for p in Protocol::ALL {
            assert_eq!(Protocol::parse(p.as_str()), Some(*p));
        }
    }

    #[test]
    fn provider_detail_flattens_summary() {
        let json = serde_json::json!({
            "id": "x", "name": "X", "group": "oauth", "status": "unsupported",
            "connectionCount": 0, "adapterReady": false, "protocols": ["gemini"], "quotaHint": null,
            "serviceKinds": ["llm"], "website": null, "color": null, "textIcon": null,
            "authModes": ["oauth"], "pendingReason": "OAuth 授权流程待接入",
            "riskNotice": null, "connections": [],
            "distribution": {"roundRobin": true, "maxConcurrency": 4, "maxRetries": 2},
            "customHeaders": [], "models": [], "thinkingLevel": "auto", "baseUrl": null
        });
        let d: ProviderDetail = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(d.summary.id, "x");
        let back = serde_json::to_value(&d).unwrap();
        assert_eq!(back, json);
    }

    #[test]
    fn secret_result_debug_is_redacted() {
        let r = ApiKeySecretResult {
            key: ApiKeyInfo {
                id: "1".into(),
                name: "n".into(),
                masked: "sk-rd-…abcd".into(),
                secret: None,
                protocols: vec![],
                allowed_models: vec![],
                rpm_limit: None,
                tpm_limit: None,
                total_limit: None,
                call_count: 0,
                created_at: "t".into(),
                last_used_at: None,
                expires_at: None,
                disabled: false,
            },
            secret: "sk-rd-supersecret".into(),
        };
        assert!(!format!("{r:?}").contains("supersecret"));
    }
}
