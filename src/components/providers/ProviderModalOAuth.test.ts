import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import type { Connection, OAuthProviderInfo, ProviderDetail, ProviderSummary } from '@/types'

const apiMock = vi.hoisted(() => ({
  listProviders: vi.fn(),
  getProvider: vi.fn(),
  saveProvider: vi.fn(),
  testConnection: vi.fn(),
  testProviders: vi.fn(),
  createCustomProvider: vi.fn(),
  deleteCustomProvider: vi.fn(),
  addConnection: vi.fn(),
  updateConnectionCredential: vi.fn(),
  oauthListProviders: vi.fn(),
  oauthSetClientCredentials: vi.fn(),
  oauthClearClientCredentials: vi.fn(),
  oauthStart: vi.fn(),
  oauthPoll: vi.fn(),
  oauthCancel: vi.fn(),
  oauthReauthorize: vi.fn(),
  oauthDisconnect: vi.fn(),
}))
vi.mock('@/api', () => ({ api: apiMock }))
vi.mock('@/api/openExternal', () => ({ openExternal: vi.fn().mockResolvedValue(undefined) }))

import { OAuthApiError, oauthErrorText } from '@/api/oauthError'
import { __resetOAuthMock, __setOAuthMockTiming, oauthMock } from '@/api/mock/oauth'
import BaseModal from '@/components/BaseModal.vue'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import ProviderModal from './ProviderModal.vue'

const summary: ProviderSummary = {
  id: 'claude',
  name: 'Claude Code',
  group: 'oauth',
  status: 'disconnected',
  connectionCount: 0,
  adapterReady: true,
  protocols: ['anthropic-messages'],
  quotaHint: null,
  serviceKinds: ['llm'],
  website: null,
  color: null,
  textIcon: null,
  authModes: ['oauth', 'apiKey'],
  pendingReason: null,
}

const oauthConn = (over: Partial<Connection> = {}): Connection => ({
  id: 'oc1', providerId: 'claude', accountLabel: 'me@example.com', kind: 'oauth', status: 'active', priority: 1, weight: 1, enabled: true, ...over,
})

function detail(over: Partial<ProviderDetail> = {}): ProviderDetail {
  return {
    ...summary,
    riskNotice: null,
    connections: [],
    distribution: { roundRobin: true, maxConcurrency: 4, maxRetries: 2 },
    customHeaders: [],
    models: [],
    thinkingLevel: 'auto',
    ...over,
  }
}

let infos: OAuthProviderInfo[] = []

async function open(d: ProviderDetail, prov: ProviderSummary = summary) {
  apiMock.getProvider.mockResolvedValue(d)
  const w = mount(ProviderModal, {
    props: { modelValue: true, provider: prov },
    global: { plugins: [createPinia()], stubs: { teleport: true } },
  })
  await flushPromises()
  return w
}

const btn = (w: VueWrapper, text: string) => w.findAll('button').find((b) => b.text().includes(text))
const disconnectDialog = (w: VueWrapper) => w.findAllComponents(ConfirmDialog).find((c) => c.props('title') === '断开 OAuth 连接')!

beforeEach(async () => {
  vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'Date'] })
  setActivePinia(createPinia())
  Object.values(apiMock).forEach((f) => f.mockReset())
  apiMock.listProviders.mockResolvedValue([])
  apiMock.oauthCancel.mockResolvedValue(undefined)
  __resetOAuthMock()
  __setOAuthMockTiming({ latencyMs: 0 })
  infos = await oauthMock.oauthListProviders()
  apiMock.oauthListProviders.mockResolvedValue(infos)
})
afterEach(() => vi.useRealTimers())

describe('ProviderModal：OAuth 入口', () => {
  it('OAuth 分类仅显示“登录验证”，不展示 API Key 添加入口', async () => {
    const w = await open(detail())
    expect(apiMock.oauthListProviders).toHaveBeenCalledTimes(1)
    expect(btn(w, '添加 API Key')).toBeUndefined()
    expect(btn(w, '登录验证')!.attributes('disabled')).toBeUndefined()
    expect(w.text()).not.toContain('授权流程待接入')
    expect(w.find('section[aria-label="上游地址"]').exists()).toBe(false)
    const sections = w.findAll('section[aria-label]').map((s) => s.attributes('aria-label'))
    expect(sections.indexOf('可用模型')).toBeLessThan(sections.indexOf('分发策略'))
    expect(sections.indexOf('分发策略')).toBeLessThan(sections.indexOf('Custom Headers'))
  })

  it('只支持 API Key 的提供商不拉取 OAuth 信息、没有 OAuth 按钮', async () => {
    const w = await open(detail({ authModes: ['apiKey'] }), { ...summary, authModes: ['apiKey'] })
    expect(apiMock.oauthListProviders).not.toHaveBeenCalled()
    expect(btn(w, '登录验证')).toBeUndefined()
  })

  it('适配器未就绪时详情仍如实提示转发限制，模型区只读', async () => {
    const d = detail({ adapterReady: false, status: 'unsupported', pendingReason: 'OAuth 授权流程待接入' })
    const w = await open(d, { ...summary, adapterReady: false, status: 'unsupported' })
    expect(btn(w, '登录验证')!.attributes('disabled')).toBeUndefined()
    expect(btn(w, '添加 API Key')).toBeUndefined()
    expect(w.find('[data-testid="oauth-forward-note"]').text()).toContain('授权后暂不能用于转发')
    expect(w.find('[data-testid="oauth-forward-note"]').text()).toContain('OAuth 授权流程待接入')
    expect(btn(w, '保存修改')!.attributes('disabled')).toBeDefined()
  })

  it.each([
    ['cursor', '逆向协议'],
    ['kiro', 'AWS SSO OIDC'],
  ])('custom 提供商 %s：按钮置灰，原因与接入要求可见', async (id, snippet) => {
    const s = { ...summary, id, name: id, adapterReady: false, status: 'unsupported' as const }
    const w = await open(detail({ ...s, id }), s)
    expect(btn(w, '登录验证')!.attributes('disabled')).toBeDefined()
    expect(w.find('[data-testid="oauth-custom-reason"]').text()).toContain(snippet)
    expect(w.find('[data-testid="oauth-custom-notice"]').text()).toContain('接入需要')
    await btn(w, '登录验证')!.trigger('click')
    expect(w.find('section[aria-label$="OAuth 授权"]').exists()).toBe(false)
  })
})

describe('ProviderModal：授权流程', () => {
  const started = {
    sessionId: 's1', flow: 'device' as const, authorizeUrl: null, userCode: 'ABCD-1234',
    verificationUri: 'https://example.com/device', expiresAt: '',
  }

  it('授权成功后刷新列表与详情，弹窗保持“未修改”并显示新连接', async () => {
    const w = await open(detail())
    apiMock.oauthStart.mockResolvedValue({ ...started, expiresAt: new Date(Date.now() + 300_000).toISOString() })
    apiMock.oauthPoll.mockResolvedValue({ status: 'succeeded', connectionId: 'oc1' })
    await btn(w, '登录验证')!.trigger('click')
    expect(w.find('section[aria-label="Claude Code OAuth 授权"]').exists()).toBe(true)
    await flushPromises()
    expect(apiMock.oauthStart).toHaveBeenCalledWith('claude', '')
    expect(w.find('[data-testid="oauth-user-code"]').text()).toBe('ABCD-1234')

    apiMock.listProviders.mockClear()
    apiMock.getProvider.mockResolvedValue(detail({ status: 'connected', connectionCount: 1, connections: [oauthConn()] }))
    apiMock.listProviders.mockResolvedValue([{ ...summary, status: 'connected', connectionCount: 1 }])
    await vi.advanceTimersByTimeAsync(1500)
    await flushPromises()

    expect(apiMock.listProviders).toHaveBeenCalled()
    expect(apiMock.getProvider).toHaveBeenCalledTimes(2)
    expect(w.text()).toContain('me@example.com')
    expect(w.findComponent(BaseModal).props('dirty')).toBe(false)
    expect(btn(w, '保存修改')!.attributes('disabled')).toBeDefined()
    expect(w.text()).toContain('已通过 OAuth 授权')
  })

  it('授权码流程失败时只显示映射文案', async () => {
    const w = await open(detail())
    apiMock.oauthStart.mockRejectedValue(new OAuthApiError('listen_failed'))
    await btn(w, '登录验证')!.trigger('click')
    await flushPromises()
    expect(w.text()).toContain(oauthErrorText('listen_failed'))
  })

  it('关闭弹窗会卸载面板并取消进行中的授权会话', async () => {
    const w = await open(detail())
    apiMock.oauthStart.mockResolvedValue({ ...started, expiresAt: new Date(Date.now() + 300_000).toISOString() })
    apiMock.oauthPoll.mockResolvedValue({ status: 'pending' })
    await btn(w, '登录验证')!.trigger('click')
    await flushPromises()
    await w.setProps({ modelValue: false })
    await flushPromises()
    expect(apiMock.oauthCancel).toHaveBeenCalledWith('s1')
    const polls = apiMock.oauthPoll.mock.calls.length
    await vi.advanceTimersByTimeAsync(20_000)
    expect(apiMock.oauthPoll).toHaveBeenCalledTimes(polls)
  })
})

describe('ProviderModal：重新授权与断开', () => {
  it('needsReauth 连接：显示“需要重新授权”与按钮，不显示“活跃”；点击后走 oauthReauthorize，成功后恢复 active', async () => {
    const w = await open(detail({ connections: [oauthConn({ status: 'needsReauth' })], connectionCount: 1 }))
    expect(w.text()).toContain('需要重新授权')
    expect(w.text()).not.toContain('活跃')
    await w.find('button[aria-label="重新授权 me@example.com"]').trigger('click')
    expect(w.find('section[aria-label="Claude Code OAuth 重新授权"]').exists()).toBe(true)

    apiMock.oauthReauthorize.mockResolvedValue({
      sessionId: 'r1', flow: 'authorization-code', authorizeUrl: 'https://claude.ai/oauth/authorize', userCode: null,
      verificationUri: null, expiresAt: new Date(Date.now() + 300_000).toISOString(),
    })
    apiMock.oauthPoll.mockResolvedValue({ status: 'succeeded', connectionId: 'oc1' })
    apiMock.getProvider.mockResolvedValue(detail({ connections: [oauthConn({ status: 'active' })], connectionCount: 1 }))
    await btn(w, '开始重新授权')!.trigger('click')
    await flushPromises()
    await vi.advanceTimersByTimeAsync(1500)
    await flushPromises()

    expect(apiMock.oauthReauthorize).toHaveBeenCalledWith('oc1')
    expect(apiMock.oauthStart).not.toHaveBeenCalled()
    expect(w.text()).toContain('活跃')
    expect(w.find('button[aria-label="重新授权 me@example.com"]').exists()).toBe(false)
    expect(w.findComponent(BaseModal).props('dirty')).toBe(false)
  })

  it('OAuth 连接的“断开”：先确认（说明令牌条目与立即移除），确认后调用 oauthDisconnect 并从列表移除，不产生未保存修改', async () => {
    const w = await open(detail({ connections: [oauthConn()], connectionCount: 1 }))
    expect(w.find('button[aria-label="删除连接 me@example.com"]').exists()).toBe(false)
    await w.find('button[aria-label="断开连接 me@example.com"]').trigger('click')
    expect(apiMock.oauthDisconnect).not.toHaveBeenCalled()
    const dlg = disconnectDialog(w)
    expect(dlg.props('message')).toContain('令牌条目')
    expect(dlg.props('message')).toContain('立即移除')

    apiMock.oauthDisconnect.mockResolvedValue(undefined)
    apiMock.listProviders.mockResolvedValue([{ ...summary, connectionCount: 0 }])
    await dlg.findAll('button').find((b) => b.text() === '断开')!.trigger('click')
    await flushPromises()

    expect(apiMock.oauthDisconnect).toHaveBeenCalledWith('oc1')
    expect(w.find('button[aria-label="断开连接 me@example.com"]').exists()).toBe(false)
    expect(apiMock.listProviders).toHaveBeenCalled()
    expect(w.findComponent(BaseModal).props('dirty')).toBe(false)
    expect(w.text()).toContain('已断开连接')
  })

  it('断开失败：显示映射文案，连接保留', async () => {
    const w = await open(detail({ connections: [oauthConn()], connectionCount: 1 }))
    await w.find('button[aria-label="断开连接 me@example.com"]').trigger('click')
    apiMock.oauthDisconnect.mockRejectedValue({ code: 'storage', message: 'RAW secret text' })
    await disconnectDialog(w).findAll('button').find((b) => b.text() === '断开')!.trigger('click')
    await flushPromises()
    expect(w.text()).toContain(oauthErrorText('storage'))
    expect(w.text()).not.toContain('RAW secret text')
    expect(w.find('button[aria-label="断开连接 me@example.com"]').exists()).toBe(true)
  })

  it('API Key 连接保持原有的“删除”行为', async () => {
    const key: Connection = { ...oauthConn(), id: 'k1', kind: 'apiKey', accountLabel: 'team-key' }
    const w = await open(detail({ connections: [key], connectionCount: 1 }))
    expect(w.find('button[aria-label="删除连接 team-key"]').exists()).toBe(true)
    expect(w.find('button[aria-label="断开连接 team-key"]').exists()).toBe(false)
  })
})
