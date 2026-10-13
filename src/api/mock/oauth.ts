import type { OAuthApi } from '../types'
import type {
  OAuthClientCredentialsInput,
  OAuthFlow,
  OAuthPollResult,
  OAuthProviderInfo,
  OAuthStartResult,
} from '@/types'
import { OAuthApiError } from '../oauthError'
import { validateBaseUrl } from '@/components/providers/validation'
import {
  __mockAddOAuthConnection,
  __mockLookupConnection,
  __mockRemoveOAuthConnection,
  __onProvidersMockReset,
} from './providers'
import oauthJson from '../../../src-tauri/resources/oauth_providers.json'
import catalogJson from '../../../src-tauri/resources/provider_catalog.json'
import statusJson from '../../../src-tauri/resources/provider_status.json'

/**
 * 浏览器预览用的 OAuth mock。能力信息直接取自 Rust 侧内嵌的 oauth_providers.json
 * （与真实应用同一份数据），授权流程是一个带定时器语义的状态机：
 * pending 若干秒后 -> succeeded，并在 providers mock 里落一条 kind='oauth' 的连接。
 *
 * 红线：mock 永远不伪造令牌；state 里没有任何令牌/授权码/code_verifier；
 * 客户端凭据只记“是否已配置”的布尔值，clientSecret 字符串在校验后即丢弃。
 * 设备码是明显的假显示码（MOCK-0000），授权地址是提供商的纯 authorizeUrl（不带任何参数）。
 */

interface OAuthJsonEntry {
  id: string
  flow: OAuthFlow
  authorizeUrl?: string
  verificationUri?: string
  baseUrlDefault?: string
  clientId: string | null
  requiresClientSecret?: boolean
  riskNotice?: string
  custom?: { reason: string; requires: string[] }
}
interface CatalogEntry {
  id: string
  name: string
}
interface StatusEntry {
  adapter: string | null
  reason: string | null
}

const ENTRIES = (oauthJson as unknown as { providers: OAuthJsonEntry[] }).providers
const NAMES = new Map((catalogJson as unknown as CatalogEntry[]).map((c) => [c.id, c.name]))
const STATUS = statusJson as unknown as Record<string, StatusEntry>

const TOKEN_EXCHANGE_PENDING = new Set(['github', 'muse'])
const TOKEN_EXCHANGE_TEXT =
  '该提供商授权后的令牌兑换步骤尚未接入（GitHub：换取 Copilot 令牌；Muse：铸造 Model API Key），授权成功的连接暂不能用于转发。'
const FALLBACK_DEVICE_URI = 'https://example.com/mock-device'
const MOCK_USER_CODE = 'MOCK-0000'
const MAX_SESSIONS = 8

interface Timing {
  /** 授权从开始到 succeeded 的时间 */
  pendingMs: number
  /** 会话有效期 */
  expiresInMs: number
  /** 每个命令的模拟网络延迟 */
  latencyMs: number
}
const DEFAULT_TIMING: Timing = { pendingMs: 4000, expiresInMs: 10 * 60 * 1000, latencyMs: 80 }
let timing: Timing = { ...DEFAULT_TIMING }

/** 仅供测试：调整状态机的时间参数（只传要改的项） */
export function __setOAuthMockTiming(t: Partial<Timing>) {
  timing = { ...timing, ...t }
}

/** 已配置的客户端凭据——只存布尔值，不存任何内容 */
interface ConfiguredFlags {
  clientId: boolean
  secret: boolean
  baseUrl: boolean
}
let configured = new Map<string, ConfiguredFlags>()

interface Session {
  providerId: string
  label: string
  reauthId: string | null
  createdAt: number
  expiresAt: number
  state: 'pending' | 'cancelled' | 'succeeded'
  connectionId: string | null
}
let sessions = new Map<string, Session>()
let seq = 0

/** 仅供测试：重置 mock 状态（也会在 __resetProvidersMock 时被调用） */
export function __resetOAuthMock() {
  configured = new Map()
  sessions = new Map()
  seq = 0
  timing = { ...DEFAULT_TIMING }
}
__onProvidersMockReset(__resetOAuthMock)

const delay = () => (timing.latencyMs > 0 ? new Promise<void>((r) => setTimeout(r, timing.latencyMs)) : Promise.resolve())

function entryOf(providerId: string): OAuthJsonEntry {
  const e = ENTRIES.find((x) => x.id === providerId)
  if (!e) throw new OAuthApiError('unknown_provider')
  return e
}

function clientSecretMode(e: OAuthJsonEntry): 'none' | 'optional' | 'required' {
  if (e.flow === 'custom') return 'none'
  if (e.requiresClientSecret) return 'required'
  // 没有内置 clientId 的（如 GitLab）由用户注册应用，机密应用才需要 secret：可选
  return e.clientId === null ? 'optional' : 'none'
}

function infoOf(e: OAuthJsonEntry): OAuthProviderInfo {
  const flags = configured.get(e.id)
  const base = {
    providerId: e.id,
    flow: e.flow,
    riskNotice: e.riskNotice ?? null,
  }
  if (e.flow === 'custom') {
    return {
      ...base,
      status: 'custom',
      reason: e.custom?.reason ?? null,
      requires: [...(e.custom?.requires ?? [])],
      limitation: null,
      tokenExchangePending: false,
      clientIdRequired: false,
      clientSecret: 'none',
      supportsBaseUrl: false,
      clientConfigured: false,
    }
  }
  const secretMode = clientSecretMode(e)
  const missingId = e.clientId === null && !flags?.clientId
  const missingSecret = !!e.requiresClientSecret && !flags?.secret
  const st = STATUS[e.id]
  const exchangePending = TOKEN_EXCHANGE_PENDING.has(e.id)
  const limitation = exchangePending ? TOKEN_EXCHANGE_TEXT : st && !st.adapter ? (st.reason ?? '转发适配器尚未接入') : null
  return {
    ...base,
    status: missingId || missingSecret ? 'need_client_credentials' : 'ready',
    reason: null,
    requires: [],
    limitation,
    tokenExchangePending: exchangePending,
    clientIdRequired: e.clientId === null,
    clientSecret: secretMode,
    supportsBaseUrl: !!e.baseUrlDefault,
    clientConfigured: !!flags && (flags.clientId || flags.secret || flags.baseUrl),
  }
}

function readyEntry(providerId: string): OAuthJsonEntry {
  const e = entryOf(providerId)
  if (e.flow === 'custom') throw new OAuthApiError('custom_flow_required')
  if (infoOf(e).status === 'need_client_credentials') throw new OAuthApiError('missing_client_credential')
  return e
}

function cleanLabel(raw: string): string {
  const t = raw.trim()
  if (!t) return ''
  // eslint-disable-next-line no-control-regex
  if ([...t].length > 128 || /[\u0000-\u001f\u007f]/.test(t)) throw new OAuthApiError('label_invalid')
  return t
}

function newSession(e: OAuthJsonEntry, label: string, reauthId: string | null): OAuthStartResult {
  const live = [...sessions.values()].filter((s) => s.state === 'pending' && s.expiresAt > Date.now()).length
  if (live >= MAX_SESSIONS) throw new OAuthApiError('too_many_sessions')
  const now = Date.now()
  const sessionId = `mock-oauth-session-${++seq}`
  sessions.set(sessionId, {
    providerId: e.id,
    label,
    reauthId,
    createdAt: now,
    expiresAt: now + timing.expiresInMs,
    state: 'pending',
    connectionId: null,
  })
  const expiresAt = new Date(now + timing.expiresInMs).toISOString()
  if (e.flow === 'device') {
    return {
      sessionId,
      flow: 'device',
      authorizeUrl: null,
      userCode: MOCK_USER_CODE,
      verificationUri: e.verificationUri ?? FALLBACK_DEVICE_URI,
      expiresAt,
    }
  }
  const authorizeUrl = (e.authorizeUrl ?? '').replace('{base_url}', e.baseUrlDefault ?? '')
  return { sessionId, flow: 'authorization-code', authorizeUrl, userCode: null, verificationUri: null, expiresAt }
}

function validateCredentials(e: OAuthJsonEntry, input: OAuthClientCredentialsInput): ConfiguredFlags {
  const info = infoOf(e)
  const clientId = (input.clientId ?? '').trim()
  const secret = input.clientSecret ?? ''
  const baseUrl = (input.baseUrl ?? '').trim()
  if (info.clientIdRequired && (!clientId || clientId.length > 512)) throw new OAuthApiError('invalid_input')
  if (info.clientSecret === 'required' && !secret.trim()) throw new OAuthApiError('invalid_input')
  if (info.clientSecret === 'none' && secret) throw new OAuthApiError('invalid_input')
  if (secret && (secret.length > 4096 || /\s/.test(secret))) throw new OAuthApiError('invalid_input')
  if (baseUrl) {
    if (!info.supportsBaseUrl) throw new OAuthApiError('invalid_input')
    if (!validateBaseUrl(baseUrl).ok) throw new OAuthApiError('invalid_input')
  }
  // 只记布尔值；secret / clientId / baseUrl 的内容到此为止，不被保存
  return { clientId: !!clientId, secret: !!secret, baseUrl: !!baseUrl }
}

function advance(s: Session): void {
  if (s.state !== 'pending' || Date.now() - s.createdAt < timing.pendingMs) return
  if (Date.now() >= s.expiresAt) return
  const providerName = NAMES.get(s.providerId) ?? s.providerId
  const label = s.label || `${providerName} 账户`
  const conn = __mockAddOAuthConnection(s.providerId, label, s.reauthId ?? undefined)
  if (!conn) return
  s.state = 'succeeded'
  s.connectionId = conn.id
}

export const oauthMock: OAuthApi = {
  oauthListProviders: async () => {
    await delay()
    return ENTRIES.map(infoOf)
  },

  oauthSetClientCredentials: async (input) => {
    await delay()
    const e = entryOf(input.providerId)
    if (e.flow === 'custom') throw new OAuthApiError('custom_flow_required')
    configured.set(e.id, validateCredentials(e, input))
    return infoOf(e)
  },

  oauthClearClientCredentials: async (providerId) => {
    await delay()
    const e = entryOf(providerId)
    configured.delete(e.id)
    return infoOf(e)
  },

  oauthStart: async (providerId, accountLabel) => {
    await delay()
    const e = readyEntry(providerId)
    return newSession(e, cleanLabel(accountLabel), null)
  },

  oauthPoll: async (sessionId): Promise<OAuthPollResult> => {
    await delay()
    const s = sessions.get(sessionId)
    if (!s) return { status: 'expired' }
    advance(s)
    if (s.state === 'succeeded' && s.connectionId) return { status: 'succeeded', connectionId: s.connectionId }
    if (s.state === 'cancelled') return { status: 'cancelled' }
    if (Date.now() >= s.expiresAt) return { status: 'expired' }
    return { status: 'pending' }
  },

  oauthCancel: async (sessionId) => {
    await delay()
    const s = sessions.get(sessionId)
    if (s && s.state === 'pending') s.state = 'cancelled'
  },

  oauthReauthorize: async (connectionId) => {
    await delay()
    const hit = __mockLookupConnection(connectionId)
    if (!hit) throw new OAuthApiError('not_found')
    if (hit.kind !== 'oauth') throw new OAuthApiError('not_oauth')
    const e = readyEntry(hit.providerId)
    return newSession(e, '', connectionId)
  },

  oauthDisconnect: async (connectionId) => {
    await delay()
    const r = __mockRemoveOAuthConnection(connectionId)
    if (r === 'not_found') throw new OAuthApiError('not_found')
    if (r === 'not_oauth') throw new OAuthApiError('not_oauth')
  },
}
