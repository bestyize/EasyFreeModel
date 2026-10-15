import type {
  AddConnectionInput,
  ApiKeyInfo,
  ApiKeySecretResult,
  AppSettings,
  Connection,
  CreateApiKeyInput,
  CreateCustomProviderInput,
  GatewayStatus,
  MediaJob,
  MediaKind,
  ModelEntry,
  OAuthClientCredentialsInput,
  OAuthPollResult,
  OAuthProviderInfo,
  OAuthStartResult,
  ProviderDetail,
  ProviderSummary,
  QuotaAccount,
  SystemStatus,
  TestResult,
  UsageFilter,
  UsageRange,
  UsageRequest,
  UsageSummary,
} from '@/types'

/**
 * 前端唯一的后端入口。Tauri 环境下由 src/api/tauri.ts 通过 invoke 实现，
 * 浏览器预览下由 src/api/mock 实现。命令名 = 方法名的 snake_case。
 * Rust 侧是数据唯一事实来源，前端不持有第三方密钥明文。
 */
export interface ProvidersApi {
  listProviders(): Promise<ProviderSummary[]>
  getProvider(id: string): Promise<ProviderDetail>
  saveProvider(detail: ProviderDetail): Promise<ProviderDetail>
  testConnection(connectionId: string): Promise<TestResult>
  /** 创建自定义提供商（Rust 侧校验协议与 Base URL） */
  createCustomProvider(input: CreateCustomProviderInput): Promise<ProviderDetail>
  /** 仅自定义提供商可删除，同时清理其全部凭据 */
  deleteCustomProvider(id: string): Promise<void>
  /** 录入 API Key 创建连接；返回值不含密钥 */
  addConnection(input: AddConnectionInput): Promise<Connection>
  updateConnectionCredential(connectionId: string, apiKey: string): Promise<void>
  /** 批量测试：逐个返回 TestResult（providerId 归组）；适配器未就绪的如实 ok=false + 原因 */
  testProviders(ids: string[]): Promise<TestResult[]>
  /** 从上游（或内置模型库）拉取模型列表并同步更新到数据库 */
  fetchProviderModels(providerId: string): Promise<ModelEntry[]>
  /** 模型级可用性探测：对单个已保存模型发起一次最小真实上游请求 */
  testProviderModel(providerId: string, modelId: string): Promise<TestResult>
}

export interface ApiKeysApi {
  listApiKeys(): Promise<ApiKeyInfo[]>
  createApiKey(input: CreateApiKeyInput): Promise<ApiKeySecretResult>
  rotateApiKey(id: string): Promise<ApiKeySecretResult>
  setApiKeyDisabled(id: string, disabled: boolean): Promise<ApiKeyInfo>
  deleteApiKey(id: string): Promise<void>
}

export interface UsageApi {
  usageSummary(range: UsageRange): Promise<UsageSummary>
  listUsageRequests(filter: UsageFilter): Promise<UsageRequest[]>
}

export interface QuotaApi {
  listQuotaAccounts(): Promise<QuotaAccount[]>
  refreshQuota(connectionId: string | null): Promise<QuotaAccount[]>
  setQuotaAccountEnabled(connectionId: string, enabled: boolean): Promise<void>
}

export interface SystemApi {
  systemStatus(): Promise<SystemStatus>
  getSettings(): Promise<AppSettings>
  saveSettings(settings: AppSettings): Promise<AppSettings>
  startGateway(): Promise<GatewayStatus>
  stopGateway(): Promise<GatewayStatus>
}

export interface MediaApi {
  listMediaJobs(kind: MediaKind): Promise<MediaJob[]>
  submitMediaJob(kind: MediaKind, params: Record<string, unknown>): Promise<MediaJob>
  cancelMediaJob(id: string): Promise<MediaJob>
}

/**
 * OAuth 授权（命令名 = 方法名 snake_case：oauth_list_providers 等）。
 * 失败时 Rust 侧以 `{ code, message }` 拒绝，tauri.ts 统一转成 `OAuthApiError`（见 src/api/oauthError.ts）；
 * 返回值与错误里都不含令牌、授权码、code_verifier、refresh token、client secret。
 */
export interface OAuthApi {
  /** 全部 25 家 OAuth 提供商的授权能力与状态（含 14 家 custom） */
  oauthListProviders(): Promise<OAuthProviderInfo[]>
  /** 保存用户自备的客户端凭据到系统凭据库；返回更新后的该提供商信息（只含布尔值，不含凭据内容） */
  oauthSetClientCredentials(input: OAuthClientCredentialsInput): Promise<OAuthProviderInfo>
  oauthClearClientCredentials(providerId: string): Promise<OAuthProviderInfo>
  /** 开始授权。accountLabel 为空时由后端用令牌里的邮箱或“<提供商> 账户”命名 */
  oauthStart(providerId: string, accountLabel: string): Promise<OAuthStartResult>
  /** 轮询授权结果（建议每 1–2 秒一次）。未知/已清理的会话返回 expired */
  oauthPoll(sessionId: string): Promise<OAuthPollResult>
  /** 取消授权并立即释放本地回调端口 */
  oauthCancel(sessionId: string): Promise<void>
  /** 对已有连接重新授权（成功后连接恢复为 active，凭据原位更新） */
  oauthReauthorize(connectionId: string): Promise<OAuthStartResult>
  /** 断开：删除令牌条目与连接 */
  oauthDisconnect(connectionId: string): Promise<void>
}

export type Api = ProvidersApi & OAuthApi & ApiKeysApi & UsageApi & QuotaApi & SystemApi & MediaApi
