// 前后端共享契约。Rust 侧 serde 结构体使用 #[serde(rename_all = "camelCase")] 与此对齐。

/**
 * 提供商分组。'apikey'（API Key 提供商）是对 PRODUCT_DESIGN.md §4.2「固定三组」的有意扩展：
 * 9Router 对齐后目录有 133 家，其中 87 家放不进原来的三组。
 */
export type ProviderGroup = 'custom' | 'oauth' | 'free' | 'apikey'
export type ConnectionStatus = 'connected' | 'disconnected' | 'error' | 'unsupported' | 'checking'
export type CredentialKind = 'oauth' | 'apiKey'
export type Protocol = 'openai-chat' | 'openai-responses' | 'anthropic-messages' | 'gemini'

export interface ProviderSummary {
  id: string
  name: string
  group: ProviderGroup
  status: ConnectionStatus
  connectionCount: number
  /** 适配器是否已真实接入；false 时 UI 显示“待接入”，不得伪造可用状态 */
  adapterReady: boolean
  protocols: Protocol[]
  /** 配额速览，未知时为 null，不得与 0 混淆 */
  quotaHint: string | null
  /** 目录声明的服务类型：llm / image / imageToText / tts / stt / video / webSearch / webFetch / embedding / systemone */
  serviceKinds: string[]
  /** 官网（用于“获取 Key”入口），目录没有则为 null */
  website: string | null
  /** 目录品牌色 #RRGGBB，无效/缺失为 null（Logo 回退到名称哈希色） */
  color: string | null
  /** 目录文字图标（1–3 个字符），缺失为 null（回退到名称首字母） */
  textIcon: string | null
  /** 支持的凭据方式；adapterReady=false 时即使含 apiKey 也不能添加连接 */
  authModes: ('oauth' | 'apiKey')[]
  /** adapterReady=false 时显示的“待接入”原因；就绪时为 null */
  pendingReason: string | null
}

export interface Connection {
  id: string
  providerId: string
  accountLabel: string
  kind: CredentialKind
  /**
   * needsReauth：OAuth 令牌已失效（refresh token 被撤销/过期），需要用户重新授权；
   * 此状态的连接不参与路由，也不计入“已连接”。
   */
  status: 'active' | 'paused' | 'error' | 'needsReauth'
  priority: number
  weight: number
  enabled: boolean
}

export interface ModelEntry {
  /** 稳定内部 ID：provider_id/account_id/upstream_model_id */
  id: string
  upstreamModelId: string
  publicAlias: string
  description: string
  enabled: boolean
  supportsThinking: boolean
}

export interface DistributionPolicy {
  roundRobin: boolean
  maxConcurrency: number
  maxRetries: number
}

export interface CustomHeader {
  key: string
  value: string
  secret: boolean
}

export interface ProviderDetail extends ProviderSummary {
  riskNotice: string | null
  connections: Connection[]
  distribution: DistributionPolicy
  customHeaders: CustomHeader[]
  models: ModelEntry[]
  thinkingLevel: 'auto' | 'low' | 'medium' | 'high'
  /** 上游地址（非机密）：自定义提供商为用户填写的 Base URL；无则 null/缺省 */
  baseUrl?: string | null
}

/** 自定义提供商支持的协议（不含 openai-responses） */
export type CustomProtocol = 'openai-chat' | 'anthropic-messages' | 'gemini'

export interface CreateCustomProviderInput {
  name: string
  protocol: CustomProtocol
  /** 必须是 https，或回环地址（localhost / 127.0.0.1 / ::1）上的 http */
  baseUrl: string
}

/** 录入 API Key 并创建连接。apiKey 只用于这一次提交，前端任何 store/本地存储都不得保留 */
export interface AddConnectionInput {
  providerId: string
  accountLabel: string
  apiKey: string
}

export interface TestResult {
  /** 提供商未就绪、没有连接时为空串 */
  connectionId: string
  /** 结果所属提供商（批量测试按它归组） */
  providerId?: string | null
  ok: boolean
  latencyMs: number | null
  error: string | null
}

// ---- OAuth 授权（框架见 src-tauri/src/oauth；命令见 src-tauri/src/commands/oauth.rs）----
//
// 红线：令牌、授权码、code_verifier、refresh token、client secret 绝不进入前端 store、mock、
// 命令返回值或事件负载。前端只能拿到：授权 URL、设备码页面的 user_code 与 verification_uri、
// 状态枚举、脱敏错误码。client secret 只出现在“配置客户端凭据”表单的本地输入框里，
// 提交的瞬间清空，且只作为 oauth_set_client_credentials 这一次调用的入参。

/** 授权流程类型；custom = 依赖专有协议，本版本不实现（按钮置灰并显示原因） */
export type OAuthFlow = 'authorization-code' | 'device' | 'custom'
/**
 * ready：可以直接开始授权；
 * need_client_credentials：需要用户先配置客户端凭据（界面文案「需要配置客户端凭据」）；
 * custom：专有流程，不支持（reason 说明原因，requires 说明接入需要什么）。
 */
export type OAuthProviderStatus = 'ready' | 'need_client_credentials' | 'custom'

export interface OAuthProviderInfo {
  providerId: string
  flow: OAuthFlow
  status: OAuthProviderStatus
  /** status=custom 时：为什么不做（取自 oauth_providers.json，面向用户）；其余为 null */
  reason: string | null
  /** status=custom 时：接入需要具备什么；其余为空数组 */
  requires: string[]
  /**
   * 授权成功后在转发环节的限制（如实展示，不得隐藏）：
   * - github / muse：“该提供商授权后的令牌兑换步骤尚未接入……”
   * - 转发适配器未接入的提供商：该提供商的待接入原因
   * 没有限制时为 null。
   */
  limitation: string | null
  /** 授权后的令牌兑换步骤（Copilot 令牌 / Model API Key）尚未接入：true 时连接可授权但不能用于转发 */
  tokenExchangePending: boolean
  /** 风险提示（借用官方客户端授权通道等），无则 null */
  riskNotice: string | null
  /** “配置客户端凭据”表单要显示哪些字段（custom 时全为 false/'none'） */
  clientIdRequired: boolean
  clientSecret: 'none' | 'optional' | 'required'
  /** 支持覆盖基础地址（自托管实例，例如 GitLab） */
  supportsBaseUrl: boolean
  /** 凭据库里是否已有用户配置的客户端凭据（只是布尔值，不含任何内容） */
  clientConfigured: boolean
}

export interface OAuthStartResult {
  sessionId: string
  flow: 'authorization-code' | 'device'
  /** authorization-code：交给系统浏览器打开的授权地址；device 为 null */
  authorizeUrl: string | null
  /** device：展示给用户输入的短码；authorization-code 为 null */
  userCode: string | null
  /** device：用户要打开的验证页面；authorization-code 为 null */
  verificationUri: string | null
  /** 会话过期时间（ISO 8601） */
  expiresAt: string
}

export type OAuthPollResult =
  | { status: 'pending' }
  | { status: 'succeeded'; connectionId: string }
  /** code 是脱敏的错误码，见 src/api/oauthError.ts */
  | { status: 'failed'; code: string }
  | { status: 'cancelled' }
  | { status: 'expired' }

/** 提交客户端凭据的入参。clientSecret 只用于这一次调用，调用方须在提交的同时清空输入框 */
export interface OAuthClientCredentialsInput {
  providerId: string
  clientId: string
  clientSecret?: string
  baseUrl?: string
}

// ---- API 密钥 ----
export interface ApiKeyInfo {
  id: string
  name: string
  /** 仅含前后掩码，例如 sk-r…a9f2 */
  masked: string
  /** 完整密钥，可供关闭后再次查看与复制 */
  secret?: string | null
  protocols: Protocol[]
  allowedModels: string[]
  rpmLimit: number | null
  tpmLimit: number | null
  totalLimit: number | null
  callCount: number
  createdAt: string
  lastUsedAt: string | null
  expiresAt: string | null
  disabled: boolean
}

export interface CreateApiKeyInput {
  name: string
  expiresAt: string | null
  protocols: Protocol[]
  allowedModels: string[]
  rpmLimit: number | null
  tpmLimit: number | null
  totalLimit: number | null
}

/** 完整密钥只在创建/轮换时返回这一次 */
export interface ApiKeySecretResult {
  key: ApiKeyInfo
  secret: string
}

// ---- 使用情况 ----
export type UsageRange = 'today' | '24h' | '7d' | '30d' | 'all'

export interface UsageMetric {
  value: number
  /** 较上一周期变化百分比，无对比数据为 null */
  deltaPct: number | null
}

export interface UsageSummary {
  range: UsageRange
  requests: UsageMetric
  inputTokens: UsageMetric
  outputTokens: UsageMetric
  cachedTokens: UsageMetric
  estimatedCost: UsageMetric
  /** 成本估算来源说明，必须在 UI 标注 */
  costSource: string
  trend: { label: string; input: number; output: number; cached: number; requests: number; cost: number }[]
  providerShare: { providerId: string; name: string; tokens: number; pct: number }[]
  modelRanking: { model: string; providerId: string; tokens: number; pct: number }[]
  requestDistribution: { providerId: string; name: string; requests: number; pct: number }[]
}

export interface UsageRequest {
  requestId: string
  providerId: string
  accountId: string
  model: string
  inputTokens: number
  outputTokens: number
  cachedTokens: number
  latencyMs: number
  status: 'success' | 'error' | 'cancelled'
  estimatedCost: number
  /** Token 数据来源：上游返回或本地估算 */
  tokenSource: 'upstream' | 'estimated'
  timestamp: string
}

export interface UsageFilter {
  range: UsageRange
  providerId?: string
  model?: string
  status?: UsageRequest['status']
  protocol?: Protocol
  query?: string
}

// ---- 配额 ----
export interface QuotaPeriod {
  /** 保留上游语义，如 5h / Weekly / Monthly / Bonus Pack 1 */
  name: string
  unit: string
  used: number | null
  total: number | null
  remaining: number | null
  resetsAt: string | null
  /** 实时值 / 缓存值 / 估算值 / 未知 */
  source: 'live' | 'cached' | 'estimated' | 'unknown'
}

export interface QuotaAccount {
  connectionId: string
  providerId: string
  providerName: string
  accountLabel: string
  enabled: boolean
  periods: QuotaPeriod[]
  /** 只提供货币余额的上游 */
  balances: { currency: string; amount: number | null }[]
  lastSyncedAt: string | null
  error: string | null
}

// ---- 设置与服务 ----
export interface GatewayStatus {
  running: boolean
  address: string
  port: number
  error: string | null
}

export interface SystemStatus {
  gateway: GatewayStatus
  apiRequests: number
  successRate: number | null
  avgLatencyMs: number | null
}

export interface AppSettings {
  listenAddress: string
  port: number
  autoStart: boolean
  trayEnabled: boolean
  timeoutSeconds: number
  retryCount: number
  language: 'zh-CN' | 'en'
  theme: 'light' | 'dark' | 'system'
  fontScale: number
  redactLogs: boolean
}

// ---- 多模态作业 ----
export type MediaKind = 'image' | 'asr' | 'tts' | 'video' | 'search'
export type JobStatus = 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled'

export interface MediaJob {
  id: string
  kind: MediaKind
  providerId: string
  model: string
  status: JobStatus
  summary: string
  outputPath: string | null
  error: string | null
  createdAt: string
  finishedAt: string | null
}
