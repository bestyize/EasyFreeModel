import type { ProvidersApi } from '../types'
import type {
  Connection,
  CredentialKind,
  ModelEntry,
  Protocol,
  ProviderDetail,
  ProviderGroup,
  ProviderSummary,
  TestResult,
} from '@/types'
import { validateBaseUrl } from '@/components/providers/validation'
import catalogJson from '../../../src-tauri/resources/provider_catalog.json'
import statusJson from '../../../src-tauri/resources/provider_status.json'

/**
 * 浏览器预览用 mock，仅在非 Tauri 环境生效。
 *
 * 目录直接读取 Rust 侧内嵌的同两份资源（provider_catalog.json / provider_status.json），
 * 与真实应用一致：133 家，adapterReady 由 provider_status.json 的 adapter 是否为 null 决定，
 * 待接入原因取自 reason。规则与 Rust 的 providers::catalog 保持一致（分组、默认服务类型、色值规范化）。
 *
 * mock 同样遵守红线：API Key 只在 addConnection / updateConnectionCredential 的入参里出现一次，
 * 校验后即丢弃，不写入任何状态、日志或返回值。除 openrouter 的演示连接外，不预置任何账户，
 * 且 testConnection / testProviders 永远不会伪造“通过”。
 */

interface CatalogJsonEntry {
  id: string
  name: string
  category: 'oauth' | 'apikey' | 'freeTier' | 'free' | 'webCookie'
  color: string | null
  textIcon: string | null
  website: string | null
  format: string | null
  hasOAuth: boolean
  authModes: string[]
  serviceKinds: string[]
  riskNotice: string | null
}
interface StatusJsonEntry {
  adapter: string | null
  baseUrl: string | null
  auth: unknown
  reason: string | null
}

const CATALOG = catalogJson as unknown as CatalogJsonEntry[]
const STATUS = statusJson as unknown as Record<string, StatusJsonEntry>

export const SECRET_MASK = '••••••••'
const RISK_NOTICE =
  '该接入方式依赖 OAuth/订阅账户，可能受上游服务条款限制。仅在你本人授权且上游允许的前提下使用，不要用于规避服务限制。'

const GROUP_RANK: Record<ProviderGroup, number> = { custom: 0, oauth: 1, free: 2, apikey: 3 }

function groupOf(category: CatalogJsonEntry['category']): ProviderGroup {
  if (category === 'oauth') return 'oauth'
  if (category === 'free' || category === 'freeTier') return 'free'
  return 'apikey' // apikey 与 webCookie
}

function protocolsOf(format: string | null): Protocol[] {
  switch (format) {
    case 'openai':
      return ['openai-chat']
    case 'claude':
      return ['anthropic-messages']
    case 'openai-responses':
      return ['openai-responses']
    case 'gemini':
    case 'vertex':
      return ['gemini']
    default:
      return []
  }
}

function authModesOf(modes: string[]): CredentialKind[] {
  const out: CredentialKind[] = []
  for (const m of modes) {
    const k = m.toLowerCase() === 'oauth' ? 'oauth' : m.toLowerCase() === 'apikey' ? 'apiKey' : null
    if (k && !out.includes(k)) out.push(k)
  }
  return out.length ? out : ['apiKey']
}

/** `#RRGGBB`；目录里有 `#ffffffff` 之类 8 位写法，取前 6 位；无效为 null（与 Rust 一致） */
export function normalizeColor(raw: string | null | undefined): string | null {
  const m = /^#([0-9a-f]{6})([0-9a-f]{2})?$/i.exec((raw ?? '').trim())
  return m ? `#${m[1].toUpperCase()}` : null
}

function blankDetail(e: CatalogJsonEntry): ProviderDetail {
  const st = STATUS[e.id]
  const ready = !!st?.adapter
  const group = groupOf(e.category)
  const oauthLike = e.category === 'oauth' || (e.hasOAuth && !!e.riskNotice)
  return {
    id: e.id,
    name: e.name,
    group,
    status: ready ? 'disconnected' : 'unsupported',
    connectionCount: 0,
    adapterReady: ready,
    protocols: protocolsOf(e.format),
    quotaHint: null,
    // 未声明服务类型的（azure / chutes / bai / gitlab）按 llm 处理
    serviceKinds: e.serviceKinds.length ? [...e.serviceKinds] : ['llm'],
    website: e.website,
    color: normalizeColor(e.color),
    textIcon: e.textIcon,
    authModes: authModesOf(e.authModes),
    pendingReason: ready ? null : (st?.reason ?? '该提供商尚未登记适配器状态，暂不可用'),
    riskNotice: oauthLike ? RISK_NOTICE : null,
    connections: [],
    distribution: { roundRobin: true, maxConcurrency: 4, maxRetries: 2 },
    customHeaders: [],
    models: [],
    thinkingLevel: 'auto',
    baseUrl: st?.baseUrl ?? null,
  }
}

function model(
  providerId: string,
  accountId: string,
  upstream: string,
  alias: string,
  description: string,
  supportsThinking: boolean,
  enabled = true,
): ModelEntry {
  return {
    id: `${providerId}/${accountId}/${upstream}`,
    upstreamModelId: upstream,
    publicAlias: alias,
    description,
    enabled,
    supportsThinking,
  }
}

function apiKeyConn(providerId: string, id: string, label: string, priority: number): Connection {
  return { id, providerId, accountLabel: label, kind: 'apiKey', status: 'active', priority, weight: 1, enabled: true }
}

/** 已通过 addConnection / updateConnectionCredential 录入过密钥的连接 id（只存 id，不存密钥） */
let credentialed = new Set<string>()
let seq = 0

function buildCatalog(): Map<string, ProviderDetail> {
  const map = new Map<string, ProviderDetail>()
  for (const e of CATALOG) map.set(e.id, blankDetail(e))

  // 演示数据（仅预览）：OpenRouter 适配器就绪，放一条演示连接与两个模型，不代表真实连接状态
  const or = map.get('openrouter')
  if (or?.adapterReady) {
    or.status = 'connected'
    or.quotaHint = '免费额度（未知用量）'
    or.connections = [apiKeyConn('openrouter', 'or-1', 'demo-key (sk-or…demo)', 1)]
    or.connectionCount = 1
    or.models = [
      model('openrouter', 'or-1', 'meta-llama/llama-free', 'or/llama-free', 'Llama (free)', false),
      model('openrouter', 'or-1', 'qwen/qwen-free', 'or/qwen-free', 'Qwen (free)', true),
    ]
    credentialed.add('or-1')
  }
  return map
}

let store = buildCatalog()

function clone<T>(v: T): T {
  return JSON.parse(JSON.stringify(v)) as T
}

function toSummary(d: ProviderDetail): ProviderSummary {
  return {
    id: d.id,
    name: d.name,
    group: d.group,
    status: d.status,
    connectionCount: d.connections.length,
    adapterReady: d.adapterReady,
    protocols: [...d.protocols] as Protocol[],
    quotaHint: d.quotaHint,
    serviceKinds: [...d.serviceKinds],
    website: d.website,
    color: d.color,
    textIcon: d.textIcon,
    authModes: [...d.authModes],
    pendingReason: d.adapterReady ? null : d.pendingReason,
  }
}

let latency = 120
const delay = (ms?: number) => {
  const t = latency === 0 ? 0 : (ms ?? latency)
  return t > 0 ? new Promise<void>((r) => setTimeout(r, t)) : Promise.resolve()
}

function pendingText(d: ProviderDetail): string {
  return d.pendingReason?.trim() || '适配器尚未接入'
}

const MOCK_PROBE_NOTE = '配置与凭据检查通过，但预览模式（mock）不进行真实上游探测，无法确认可用'

function checkConnection(d: ProviderDetail, c: Connection): TestResult {
  const fail = (error: string): TestResult => ({
    connectionId: c.id,
    providerId: d.id,
    ok: false,
    latencyMs: null,
    error,
  })
  if (!d.adapterReady) return fail(`该提供商的适配器尚未接入，无法测试连接（${pendingText(d)}）`)
  if (c.kind !== 'apiKey') return fail('该连接不是 API Key 连接，当前无法测试')
  if (!credentialed.has(c.id)) return fail('连接缺少 API Key，请重新录入')
  return fail(MOCK_PROBE_NOTE)
}

function cleanApiKey(raw: string): string {
  const k = raw.trim()
  if (!k) throw new Error('API Key 不能为空')
  if (k.length > 4096) throw new Error('API Key 过长（超过 4096 字符）')
  if (/\s/.test(k)) throw new Error('API Key 含有空白或控制字符，请检查是否复制完整')
  return k
}

export const providersMock: ProvidersApi = {
  listProviders: async () => {
    await delay()
    return [...store.values()]
      .map((d, i) => ({ d, i }))
      .sort((a, b) => GROUP_RANK[a.d.group] - GROUP_RANK[b.d.group] || a.i - b.i)
      .map(({ d }) => toSummary(d))
  },
  getProvider: async (id) => {
    await delay()
    const d = store.get(id)
    if (!d) throw new Error(`provider not found: ${id}`)
    return clone(d)
  },
  saveProvider: async (detail) => {
    await delay()
    const prev = store.get(detail.id)
    if (!prev) throw new Error(`provider not found: ${detail.id}`)
    // 待接入的适配器不接受配置写入
    if (!prev.adapterReady) throw new Error(`该提供商适配器待接入，无法保存：${pendingText(prev)}`)
    const next = clone(prev)
    // 只接受可编辑部分；status / adapterReady / 目录字段由服务端掌控
    next.connections = clone(detail.connections).map((c) => ({ ...c, providerId: prev.id, id: c.id || `c-${++seq}` }))
    next.distribution = clone(detail.distribution)
    next.models = clone(detail.models)
    next.thinkingLevel = detail.thinkingLevel
    next.customHeaders = clone(detail.customHeaders).map((h) => (h.secret ? { ...h, value: SECRET_MASK } : h))
    if (prev.group === 'custom') {
      next.name = detail.name.trim()
      next.protocols = [...detail.protocols]
    }
    next.connectionCount = next.connections.length
    // 状态只能由真实检测得出：没有可用连接时不得显示已连接
    next.status = next.connections.some((c) => c.enabled && c.status === 'active') ? 'connected' : 'disconnected'
    const kept = new Set(next.connections.map((c) => c.id))
    for (const c of prev.connections) if (!kept.has(c.id)) credentialed.delete(c.id)
    store.set(next.id, next)
    return clone(next)
  },
  testConnection: async (connectionId): Promise<TestResult> => {
    await delay(300)
    for (const d of store.values()) {
      const c = d.connections.find((x) => x.id === connectionId)
      if (c) return checkConnection(d, c)
    }
    return { connectionId, providerId: null, ok: false, latencyMs: null, error: '连接不存在或尚未保存' }
  },

  createCustomProvider: async (input) => {
    await delay()
    const name = input.name.trim()
    if (!name || [...name].length > 64) throw new Error('提供商名称需为 1–64 个字符')
    if (!['openai-chat', 'anthropic-messages', 'gemini'].includes(input.protocol)) {
      throw new Error('协议仅支持 openai-chat / anthropic-messages / gemini')
    }
    const url = validateBaseUrl(input.baseUrl)
    if (!url.ok) throw new Error(url.message)
    const id = `custom-mock-${++seq}`
    const d: ProviderDetail = {
      id,
      name,
      group: 'custom',
      status: 'disconnected',
      connectionCount: 0,
      adapterReady: true,
      protocols: [input.protocol],
      quotaHint: null,
      serviceKinds: ['llm'],
      website: null,
      color: null,
      textIcon: null,
      authModes: ['apiKey'],
      pendingReason: null,
      riskNotice: null,
      connections: [],
      distribution: { roundRobin: true, maxConcurrency: 4, maxRetries: 2 },
      customHeaders: [],
      models: [],
      thinkingLevel: 'auto',
      baseUrl: url.url,
    }
    store.set(id, d)
    return clone(d)
  },
  deleteCustomProvider: async (id) => {
    await delay()
    const d = store.get(id)
    if (!d) throw new Error(`provider not found: ${id}`)
    if (d.group !== 'custom') throw new Error('只能删除自定义提供商，内置提供商不可删除')
    for (const c of d.connections) credentialed.delete(c.id)
    store.delete(id)
  },
  addConnection: async (input) => {
    await delay()
    const d = store.get(input.providerId)
    if (!d) throw new Error(`provider not found: ${input.providerId}`)
    if (!d.adapterReady) throw new Error(`${d.name} 暂不能添加连接：${pendingText(d)}`)
    if (!d.authModes.includes('apiKey')) throw new Error(`${d.name} 不支持 API Key 方式`)
    const label = input.accountLabel.trim()
    if (!label || [...label].length > 128) throw new Error('账户标识需为 1–128 个字符')
    cleanApiKey(input.apiKey) // 只校验，随即丢弃：mock 不保存密钥
    const priority = d.connections.reduce((m, c) => Math.max(m, c.priority), 0) + 1
    const conn = apiKeyConn(d.id, `mock-c-${++seq}`, label, priority)
    d.connections.push(conn)
    d.connectionCount = d.connections.length
    d.status = 'connected'
    credentialed.add(conn.id)
    return { ...conn }
  },
  updateConnectionCredential: async (connectionId, apiKey) => {
    await delay()
    cleanApiKey(apiKey)
    for (const d of store.values()) {
      const c = d.connections.find((x) => x.id === connectionId)
      if (!c) continue
      if (c.kind !== 'apiKey') throw new Error('只有 API Key 连接可以更新密钥')
      credentialed.add(c.id)
      return
    }
    throw new Error(`connection not found: ${connectionId}`)
  },
  testProviders: async (ids) => {
    await delay(300)
    if (ids.length > 200) throw new Error('一次最多测试 200 个提供商')
    const out: TestResult[] = []
    const fail = (providerId: string, error: string): TestResult => ({
      connectionId: '',
      providerId,
      ok: false,
      latencyMs: null,
      error,
    })
    for (const id of new Set(ids)) {
      const d = store.get(id)
      if (!d) out.push(fail(id, '提供商不存在'))
      else if (!d.adapterReady) out.push(fail(id, `该提供商的适配器尚未接入，无法测试连接（${pendingText(d)}）`))
      else if (!d.connections.length) out.push(fail(id, '尚未添加连接'))
      else {
        const enabled = d.connections.filter((c) => c.enabled)
        if (!enabled.length) out.push(fail(id, '没有已启用的连接'))
        for (const c of enabled) out.push(checkConnection(d, c))
      }
    }
    return out
  },
}

/** 其它 mock（如 oauth）注册的重置回调，使 __resetProvidersMock 能一并清理它们的会话状态 */
const resetHooks: Array<() => void> = []
export function __onProvidersMockReset(fn: () => void) {
  resetHooks.push(fn)
}

/** 仅供测试：重置 mock 状态 */
export function __resetProvidersMock() {
  credentialed = new Set()
  seq = 0
  store = buildCatalog()
  for (const fn of resetHooks) fn()
}

/** 仅供测试：调整（或关闭）模拟网络延迟 */
export function __setProvidersMockLatency(ms: number) {
  latency = ms
}

// ---- 供 OAuth mock 使用的小助手（只处理连接元数据，永远不接触令牌） ----

/** 状态派生规则与 saveProvider 一致：适配器未就绪一律不是“已连接” */
function deriveStatus(d: ProviderDetail) {
  d.connectionCount = d.connections.length
  if (!d.adapterReady) d.status = 'unsupported'
  else d.status = d.connections.some((c) => c.enabled && c.status === 'active') ? 'connected' : 'disconnected'
}

function locate(connectionId: string): { d: ProviderDetail; c: Connection } | null {
  for (const d of store.values()) {
    const c = d.connections.find((x) => x.id === connectionId)
    if (c) return { d, c }
  }
  return null
}

/** 查询连接归属与类型（OAuth mock 校验重新授权/断开的入参） */
export function __mockLookupConnection(connectionId: string): { providerId: string; kind: Connection['kind'] } | null {
  const hit = locate(connectionId)
  return hit ? { providerId: hit.d.id, kind: hit.c.kind } : null
}

/**
 * OAuth 授权成功后在 mock 里落一条 OAuth 连接（status 'active'）。
 * 传入 reauthId 时不新建，而是把该 needsReauth 连接恢复为 active。
 * 提供商或连接不存在/类型不符返回 null。
 */
export function __mockAddOAuthConnection(providerId: string, label: string, reauthId?: string): Connection | null {
  if (reauthId) {
    const hit = locate(reauthId)
    if (!hit || hit.c.kind !== 'oauth') return null
    hit.c.status = 'active'
    deriveStatus(hit.d)
    return { ...hit.c }
  }
  const d = store.get(providerId)
  if (!d) return null
  const priority = d.connections.reduce((m, c) => Math.max(m, c.priority), 0) + 1
  const conn: Connection = {
    id: `mock-oauth-${++seq}`,
    providerId: d.id,
    accountLabel: label,
    kind: 'oauth',
    status: 'active',
    priority,
    weight: 1,
    enabled: true,
  }
  d.connections.push(conn)
  deriveStatus(d)
  return { ...conn }
}

/** OAuth 断开：移除连接（对应的令牌条目在真实后端里一并删除，mock 里本来就没有令牌） */
export function __mockRemoveOAuthConnection(connectionId: string): 'ok' | 'not_found' | 'not_oauth' {
  const hit = locate(connectionId)
  if (!hit) return 'not_found'
  if (hit.c.kind !== 'oauth') return 'not_oauth'
  hit.d.connections = hit.d.connections.filter((x) => x.id !== connectionId)
  credentialed.delete(connectionId)
  deriveStatus(hit.d)
  return 'ok'
}

/** 仅供测试：把连接状态改成指定值（例如模拟令牌失效 -> needsReauth） */
export function __mockSetConnectionStatus(connectionId: string, status: Connection['status']): boolean {
  const hit = locate(connectionId)
  if (!hit) return false
  hit.c.status = status
  deriveStatus(hit.d)
  return true
}
