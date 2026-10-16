import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import oauthJson from '../../../src-tauri/resources/oauth_providers.json'
import { __resetProvidersMock, __setProvidersMockLatency, __mockSetConnectionStatus, providersMock } from './providers'
import { __resetOAuthMock, __setOAuthMockTiming, oauthMock as mock } from './oauth'
import { OAuthApiError } from '../oauthError'

const entries = (oauthJson as unknown as { providers: { id: string; flow: string }[] }).providers
const SECRET = 'GOCSPX-mock-SUPERSECRET-42'

const rejectCode = async (p: Promise<unknown>) => {
  try {
    await p
  } catch (e) {
    expect(e).toBeInstanceOf(OAuthApiError)
    return (e as OAuthApiError).code
  }
  throw new Error('expected rejection')
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'Date'] })
  __setProvidersMockLatency(0)
  __resetProvidersMock() // 同时重置 oauth mock
  __setOAuthMockTiming({ latencyMs: 0, pendingMs: 3000 })
})
afterEach(() => vi.useRealTimers())

describe('oauth mock：能力信息取自 oauth_providers.json', () => {
  it('26 家，12 家 custom、2 家 state-poll，ids / flow 与 JSON 一致', async () => {
    const list = await mock.oauthListProviders()
    expect(list.map((i) => i.providerId)).toEqual(entries.map((e) => e.id))
    expect(list).toHaveLength(26)
    expect(list.filter((i) => i.status === 'custom')).toHaveLength(12)
    expect(list.filter((i) => i.flow === 'state-poll').map((i) => i.providerId)).toEqual(['codebuddy-cn', 'codebuddy-intl'])
    for (const i of list) expect(i.flow, i.providerId).toBe(entries.find((e) => e.id === i.providerId)!.flow)
    for (const i of list.filter((x) => x.status === 'custom')) {
      expect(i.reason, i.providerId).toBeTruthy()
      expect(i.requires.length, i.providerId).toBeGreaterThan(0)
    }
  })

  it('CodeBuddy：state 轮询流程为 ready、无需 clientId', async () => {
    const by = Object.fromEntries((await mock.oauthListProviders()).map((i) => [i.providerId, i]))
    for (const id of ['codebuddy-cn', 'codebuddy-intl']) {
      expect(by[id]).toMatchObject({ flow: 'state-poll', status: 'ready', clientIdRequired: false, clientSecret: 'none' })
    }
  })

  it('状态推导：内置 clientId 的为 ready，缺 clientId / secret 的为 need_client_credentials', async () => {
    const by = Object.fromEntries((await mock.oauthListProviders()).map((i) => [i.providerId, i]))
    expect(by.claude.status).toBe('ready')
    expect(by.github.status).toBe('ready')
    expect(by.antigravity).toMatchObject({ status: 'ready', clientIdRequired: false, clientSecret: 'none' })
    expect(by.gitlab).toMatchObject({
      status: 'need_client_credentials',
      clientIdRequired: true,
      clientSecret: 'optional',
      supportsBaseUrl: true,
    })
    expect(by.claude.riskNotice).toBeTruthy()
    expect(by.cursor).toMatchObject({ flow: 'custom', status: 'custom', clientIdRequired: false, clientSecret: 'none' })
  })

  it('github / muse 的令牌兑换尚未接入；其它提供商的限制是其待接入原因', async () => {
    const by = Object.fromEntries((await mock.oauthListProviders()).map((i) => [i.providerId, i]))
    for (const id of ['github', 'muse']) {
      expect(by[id].tokenExchangePending).toBe(true)
      expect(by[id].limitation).toContain('令牌兑换步骤尚未接入')
      expect(by[id].limitation).toContain('暂不能用于转发')
    }
    expect(by.claude.tokenExchangePending).toBe(false)
    expect(by.claude.limitation).toBeNull()
  })
})

describe('oauth mock：客户端凭据（只记布尔值）', () => {
  it('校验输入；保存后变 ready 且 clientConfigured，返回值不含凭据；清除后恢复', async () => {
    expect(await rejectCode(mock.oauthSetClientCredentials({ providerId: 'gitlab', clientId: ' ', clientSecret: SECRET }))).toBe('invalid_input')
    expect(await rejectCode(mock.oauthSetClientCredentials({ providerId: 'cursor', clientId: 'x' }))).toBe('custom_flow_required')
    expect(await rejectCode(mock.oauthSetClientCredentials({ providerId: 'nope', clientId: 'x' }))).toBe('unknown_provider')

    const info = await mock.oauthSetClientCredentials({ providerId: 'gitlab', clientId: 'my-client-id', clientSecret: SECRET })
    expect(info).toMatchObject({ providerId: 'gitlab', status: 'ready', clientConfigured: true })
    const dump = JSON.stringify([info, await mock.oauthListProviders()])
    expect(dump).not.toContain('SUPERSECRET')
    expect(dump).not.toContain('my-client-id')

    const cleared = await mock.oauthClearClientCredentials('gitlab')
    expect(cleared).toMatchObject({ status: 'need_client_credentials', clientConfigured: false })
  })

  it('GitLab：密钥可选，基础地址必须是合法 https', async () => {
    expect(await rejectCode(mock.oauthSetClientCredentials({ providerId: 'gitlab', clientId: 'c', baseUrl: 'http://gitlab.example.com' }))).toBe('invalid_input')
    const info = await mock.oauthSetClientCredentials({ providerId: 'gitlab', clientId: 'c', baseUrl: 'https://gitlab.example.com' })
    expect(info.status).toBe('ready')
  })
})

describe('oauth mock：授权状态机', () => {
  it('start 的拒绝：未知 / custom / 缺凭据 / 标识非法', async () => {
    expect(await rejectCode(mock.oauthStart('nope', ''))).toBe('unknown_provider')
    expect(await rejectCode(mock.oauthStart('kiro', ''))).toBe('custom_flow_required')
    expect(await rejectCode(mock.oauthStart('gitlab', ''))).toBe('missing_client_credential')
    expect(await rejectCode(mock.oauthStart('claude', 'x'.repeat(129)))).toBe('label_invalid')
  })

  it('授权码流程：返回纯 authorizeUrl，pending 到时间后 succeeded 并创建 oauth 连接', async () => {
    const s = await mock.oauthStart('claude', '  工作账号 ')
    expect(s).toMatchObject({ flow: 'authorization-code', userCode: null, verificationUri: null })
    expect(s.authorizeUrl).toBe('https://claude.ai/oauth/authorize')
    expect(new URL(s.authorizeUrl!).search).toBe('')
    expect(Date.parse(s.expiresAt)).toBeGreaterThan(Date.now())

    expect(await mock.oauthPoll(s.sessionId)).toEqual({ status: 'pending' })
    expect((await providersMock.getProvider('claude')).connections).toHaveLength(0)

    vi.advanceTimersByTime(3000)
    const done = await mock.oauthPoll(s.sessionId)
    expect(done.status).toBe('succeeded')
    const connectionId = (done as { connectionId: string }).connectionId

    const detail = await providersMock.getProvider('claude')
    expect(detail.connections).toHaveLength(1)
    expect(detail.connections[0]).toMatchObject({ id: connectionId, kind: 'oauth', status: 'active', accountLabel: '工作账号', enabled: true })
    // claude 适配器已就绪：标成“已连接”
    expect(detail.adapterReady).toBe(true)
    expect(detail.status).toBe('connected')
    const summary = (await providersMock.listProviders()).find((p) => p.id === 'claude')!
    expect(summary.connectionCount).toBe(1)
    expect(summary.status).toBe('connected')

    // 重复轮询结果稳定，不会重复创建连接
    expect(await mock.oauthPoll(s.sessionId)).toEqual({ status: 'succeeded', connectionId })
    expect((await providersMock.getProvider('claude')).connections).toHaveLength(1)

    // 没有任何令牌样子的内容
    const dump = JSON.stringify([s, done, detail])
    expect(dump).not.toMatch(/access_?token|refresh_?token|code_verifier|client_?secret|"code"/i)
  })

  it('设备码流程：假的展示码 MOCK-0000，验证地址取自 JSON（无则占位）；默认标识为“<名称> 账户”', async () => {
    const kimi = await mock.oauthStart('kimi', '')
    expect(kimi).toMatchObject({ flow: 'device', authorizeUrl: null, userCode: 'MOCK-0000', verificationUri: 'https://www.kimi.com/code/authorize_device' })
    const gh = await mock.oauthStart('github', '')
    expect(gh.verificationUri).toBe('https://example.com/mock-device')

    vi.advanceTimersByTime(3000)
    const r = await mock.oauthPoll(gh.sessionId)
    expect(r.status).toBe('succeeded')
    const c = (await providersMock.getProvider('github')).connections[0]
    expect(c.accountLabel).toBe('GitHub Copilot 账户')
    expect(c.kind).toBe('oauth')
  })

  it('取消后轮询返回 cancelled，且不会再创建连接；未知会话为 expired', async () => {
    const s = await mock.oauthStart('github', '')
    await mock.oauthCancel(s.sessionId)
    vi.advanceTimersByTime(10_000)
    expect(await mock.oauthPoll(s.sessionId)).toEqual({ status: 'cancelled' })
    expect((await providersMock.getProvider('github')).connections).toHaveLength(0)
    expect(await mock.oauthPoll('no-such-session')).toEqual({ status: 'expired' })
    await expect(mock.oauthCancel('no-such-session')).resolves.toBeUndefined()
  })

  it('会话过期：超过有效期仍未完成则 expired', async () => {
    __setOAuthMockTiming({ pendingMs: 60_000, expiresInMs: 5000 })
    const s = await mock.oauthStart('github', '')
    vi.advanceTimersByTime(6000)
    expect(await mock.oauthPoll(s.sessionId)).toEqual({ status: 'expired' })
  })

  it('进行中的会话数有上限', async () => {
    for (let i = 0; i < 8; i++) await mock.oauthStart('github', '')
    expect(await rejectCode(mock.oauthStart('github', ''))).toBe('too_many_sessions')
  })

  it('重新授权：needsReauth 连接成功后恢复为 active（原位更新，不新增）', async () => {
    const s = await mock.oauthStart('claude', 'a')
    vi.advanceTimersByTime(3000)
    const { connectionId } = (await mock.oauthPoll(s.sessionId)) as { connectionId: string }
    expect(__mockSetConnectionStatus(connectionId, 'needsReauth')).toBe(true)
    expect((await providersMock.getProvider('claude')).connections[0].status).toBe('needsReauth')

    const re = await mock.oauthReauthorize(connectionId)
    expect(re.flow).toBe('authorization-code')
    vi.advanceTimersByTime(3000)
    expect(await mock.oauthPoll(re.sessionId)).toEqual({ status: 'succeeded', connectionId })
    const conns = (await providersMock.getProvider('claude')).connections
    expect(conns).toHaveLength(1)
    expect(conns[0].status).toBe('active')

    expect(await rejectCode(mock.oauthReauthorize('missing'))).toBe('not_found')
    const apiKeyConn = await providersMock.addConnection({ providerId: 'openai', accountLabel: 'k', apiKey: 'sk-test-123456' })
    expect(await rejectCode(mock.oauthReauthorize(apiKeyConn.id))).toBe('not_oauth')
  })

  it('断开：移除连接；API Key 连接不能走断开', async () => {
    const s = await mock.oauthStart('github', '')
    vi.advanceTimersByTime(3000)
    const { connectionId } = (await mock.oauthPoll(s.sessionId)) as { connectionId: string }
    await mock.oauthDisconnect(connectionId)
    expect((await providersMock.getProvider('github')).connections).toHaveLength(0)
    expect(await rejectCode(mock.oauthDisconnect(connectionId))).toBe('not_found')
    const apiKeyConn = await providersMock.addConnection({ providerId: 'openai', accountLabel: 'k', apiKey: 'sk-test-123456' })
    expect(await rejectCode(mock.oauthDisconnect(apiKeyConn.id))).toBe('not_oauth')
  })

  it('__resetProvidersMock 会一并清掉授权会话与凭据标记', async () => {
    const s = await mock.oauthStart('github', '')
    await mock.oauthSetClientCredentials({ providerId: 'gitlab', clientId: 'c', baseUrl: 'https://gitlab.com' })
    __resetProvidersMock()
    __setOAuthMockTiming({ latencyMs: 0 })
    expect(await mock.oauthPoll(s.sessionId)).toEqual({ status: 'expired' })
    expect((await mock.oauthListProviders()).find((i) => i.providerId === 'gitlab')!.status).toBe('need_client_credentials')
  })

  it('__resetOAuthMock 单独可用', async () => {
    const s = await mock.oauthStart('github', '')
    __resetOAuthMock() // 同时把时间参数恢复为默认值
    __setOAuthMockTiming({ latencyMs: 0 })
    expect(await mock.oauthPoll(s.sessionId)).toEqual({ status: 'expired' })
  })
})
