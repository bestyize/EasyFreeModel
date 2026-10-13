import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import type { Connection, OAuthPollResult, OAuthProviderInfo, OAuthStartResult } from '@/types'

const h = vi.hoisted(() => ({
  api: {
    oauthListProviders: vi.fn(),
    oauthSetClientCredentials: vi.fn(),
    oauthClearClientCredentials: vi.fn(),
    oauthStart: vi.fn(),
    oauthPoll: vi.fn(),
    oauthCancel: vi.fn(),
    oauthReauthorize: vi.fn(),
    oauthDisconnect: vi.fn(),
  },
  openExternal: vi.fn(),
}))
vi.mock('@/api', () => ({ api: h.api }))
vi.mock('@/api/openExternal', () => ({ openExternal: h.openExternal }))

import { OAuthApiError, oauthErrorText } from '@/api/oauthError'
import { __resetOAuthMock, __setOAuthMockTiming, oauthMock } from '@/api/mock/oauth'
import { useOAuthStore } from '@/stores/oauth'
import OAuthPanel from './OAuthPanel.vue'

let infos: OAuthProviderInfo[] = []
const infoOf = (id: string) => infos.find((i) => i.providerId === id)!

const SECRET = 'GOCSPX-super-secret-value-123'

const codeStart = (over: Partial<OAuthStartResult> = {}): OAuthStartResult => ({
  sessionId: 's-code',
  flow: 'authorization-code',
  authorizeUrl: 'https://claude.ai/oauth/authorize',
  userCode: null,
  verificationUri: null,
  expiresAt: new Date(Date.now() + 5 * 60_000).toISOString(),
  ...over,
})
const deviceStart = (over: Partial<OAuthStartResult> = {}): OAuthStartResult => ({
  sessionId: 's-dev',
  flow: 'device',
  authorizeUrl: null,
  userCode: 'ABCD-1234',
  verificationUri: 'https://github.com/login/device',
  expiresAt: new Date(Date.now() + 5 * 60_000).toISOString(),
  ...over,
})

function mountPanel(providerId: string, extra: Record<string, unknown> = {}) {
  return mount(OAuthPanel, {
    props: { providerId, providerName: providerId, ...extra },
    global: { plugins: [createPinia()] },
  })
}
const btn = (w: VueWrapper, text: string) => w.findAll('button').find((b) => b.text().includes(text))!
const userCodeEl = (w: VueWrapper) => w.find('[data-testid="oauth-user-code"]')

beforeEach(async () => {
  vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'Date'] })
  setActivePinia(createPinia())
  Object.values(h.api).forEach((f) => f.mockReset())
  h.openExternal.mockReset()
  h.openExternal.mockResolvedValue(undefined)
  h.api.oauthCancel.mockResolvedValue(undefined)
  __resetOAuthMock()
  __setOAuthMockTiming({ latencyMs: 0 })
  infos = await oauthMock.oauthListProviders()
  h.api.oauthListProviders.mockResolvedValue(infos)
})
afterEach(() => {
  vi.useRealTimers()
})

describe('OAuthPanel：加载 / 错误 / 专有流程', () => {
  it('加载中显示状态文案', async () => {
    h.api.oauthListProviders.mockReturnValue(new Promise(() => undefined))
    const w = mountPanel('claude')
    await flushPromises()
    expect(w.find('[role="status"]').text()).toContain('正在加载授权信息')
  })

  it('加载失败显示映射后的中文错误，并可重试', async () => {
    h.api.oauthListProviders.mockRejectedValueOnce(new OAuthApiError('internal'))
    const w = mountPanel('claude')
    await flushPromises()
    expect(w.find('[role="alert"]').text()).toContain(oauthErrorText('internal'))
    h.api.oauthListProviders.mockResolvedValue(infos)
    await btn(w, '重试').trigger('click')
    await flushPromises()
    expect(btn(w, '开始授权')).toBeTruthy()
    expect(w.find('[role="alert"]').exists()).toBe(false)
  })

  it.each(['cursor', 'kiro'])('专有流程 %s：授权按钮置灰，原因与接入要求是可见文字', async (id) => {
    const w = mountPanel(id)
    await flushPromises()
    const start = btn(w, '开始授权')
    expect(start.attributes('disabled')).toBeDefined()
    const info = infoOf(id)
    expect(info.status).toBe('custom')
    expect(info.reason).toBeTruthy()
    expect(w.text()).toContain(info.reason!)
    for (const r of info.requires) expect(w.text()).toContain(r)
    await start.trigger('click')
    expect(h.api.oauthStart).not.toHaveBeenCalled()
  })
})

describe('OAuthPanel：配置客户端凭据', () => {
  it('need_client_credentials 先显示表单：密码框、中文标签，不能直接开始授权', async () => {
    const w = mountPanel('antigravity')
    await flushPromises()
    expect(w.text()).toContain('配置客户端凭据')
    expect(w.text()).toContain('需要配置客户端凭据')
    expect(w.find('input[aria-label="客户端 ID"]').exists()).toBe(true)
    expect(w.find('input[aria-label="客户端密钥"]').attributes('type')).toBe('password')
    expect(w.find('input[aria-label="基础地址"]').exists()).toBe(false)
    expect(w.findAll('button').some((b) => b.text() === '开始授权')).toBe(false)
  })

  it('GitLab：客户端密钥可选、有基础地址字段', async () => {
    const w = mountPanel('gitlab')
    await flushPromises()
    expect(w.text()).toContain('客户端密钥（可选）')
    expect(w.find('input[aria-label="基础地址"]').exists()).toBe(true)
  })

  it('提交：输入框立即清空，密钥只作为那一次调用的入参，store 状态里没有密钥，随后进入授权步骤', async () => {
    let release!: (i: OAuthProviderInfo) => void
    h.api.oauthSetClientCredentials.mockReturnValue(new Promise<OAuthProviderInfo>((r) => (release = r)))
    const w = mountPanel('antigravity')
    await flushPromises()
    await w.find('input[aria-label="客户端 ID"]').setValue('my-client-id')
    await w.find('input[aria-label="客户端密钥"]').setValue(SECRET)
    await w.find('form').trigger('submit')

    // API 还没返回：输入框已经是空的
    expect((w.find('input[aria-label="客户端密钥"]').element as HTMLInputElement).value).toBe('')
    expect(h.api.oauthSetClientCredentials).toHaveBeenCalledTimes(1)
    expect(h.api.oauthSetClientCredentials).toHaveBeenCalledWith({
      providerId: 'antigravity',
      clientId: 'my-client-id',
      clientSecret: SECRET,
    })

    release({ ...infoOf('antigravity'), status: 'ready', clientConfigured: true })
    await flushPromises()
    expect(btn(w, '开始授权')).toBeTruthy()
    expect(btn(w, '清除客户端凭据')).toBeTruthy()
    const store = useOAuthStore()
    expect(JSON.stringify(store.$state)).not.toContain(SECRET)
    expect(JSON.stringify(store.$state)).not.toContain('my-client-id')
    expect(w.html()).not.toContain(SECRET)
    expect(h.api.oauthSetClientCredentials).toHaveBeenCalledTimes(1)
  })

  it('提交失败：显示映射文案，不回显原始文本，密钥输入框保持为空', async () => {
    h.api.oauthSetClientCredentials.mockRejectedValue({ code: 'invalid_input', message: `RAW ${SECRET}` })
    const w = mountPanel('antigravity')
    await flushPromises()
    await w.find('input[aria-label="客户端 ID"]').setValue('cid')
    await w.find('input[aria-label="客户端密钥"]').setValue(SECRET)
    await w.find('form').trigger('submit')
    await flushPromises()
    expect(w.find('[role="alert"]').text()).toContain(oauthErrorText('invalid_input'))
    expect(w.html()).not.toContain(SECRET)
    expect(w.html()).not.toContain('RAW')
    expect((w.find('input[aria-label="客户端密钥"]').element as HTMLInputElement).value).toBe('')
  })

  it('必填项为空时不调用 API', async () => {
    const w = mountPanel('antigravity')
    await flushPromises()
    await w.find('form').trigger('submit')
    expect(h.api.oauthSetClientCredentials).not.toHaveBeenCalled()
    expect(w.find('[role="alert"]').text()).toContain('客户端 ID')
  })

  it('取消与卸载都会清空密钥输入', async () => {
    const w = mountPanel('antigravity')
    await flushPromises()
    const input = w.find('input[aria-label="客户端密钥"]')
    await input.setValue(SECRET)
    await btn(w, '取消').trigger('click')
    expect((input.element as HTMLInputElement).value).toBe('')
    expect(w.emitted('close')).toBeTruthy()
    w.unmount()
  })

  it('已配置凭据时可以清除', async () => {
    const configured = { ...infoOf('antigravity'), status: 'ready' as const, clientConfigured: true }
    h.api.oauthListProviders.mockResolvedValue([configured])
    h.api.oauthClearClientCredentials.mockResolvedValue({ ...configured, status: 'need_client_credentials', clientConfigured: false })
    const w = mountPanel('antigravity')
    await flushPromises()
    await btn(w, '清除客户端凭据').trigger('click')
    await flushPromises()
    expect(h.api.oauthClearClientCredentials).toHaveBeenCalledWith('antigravity')
    // 清除后回到“配置客户端凭据”
    expect(w.find('input[aria-label="客户端密钥"]').exists()).toBe(true)
  })

  it('开始授权时后端报 missing_client_credential，会切回凭据表单', async () => {
    // 能力信息说“ready”，但后端实际缺凭据（例如凭据库被清理）：以后端的拒绝为准
    h.api.oauthListProviders.mockResolvedValue([{ ...infoOf('antigravity'), status: 'ready' }])
    h.api.oauthStart.mockRejectedValue(new OAuthApiError('missing_client_credential'))
    const w = mountPanel('antigravity')
    await flushPromises()
    await btn(w, '开始授权').trigger('click')
    await flushPromises()
    expect(w.text()).toContain('配置客户端凭据')
    expect(w.find('input[aria-label="客户端密钥"]').exists()).toBe(true)
  })
})

describe('OAuthPanel：开始授权', () => {
  it('空闲态：展示风险提示与使用限制（GitHub），账户标识最长 128', async () => {
    const w = mountPanel('github')
    await flushPromises()
    expect(w.text()).toContain('风险提示')
    expect(w.find('[data-testid="oauth-limitation"]').text()).toContain('令牌兑换步骤尚未接入')
    expect(w.find('input[aria-label="账户标识"]').attributes('maxlength')).toBe('128')
    expect(btn(w, '开始授权').attributes('disabled')).toBeUndefined()
  })

  it('授权码流程：打开系统浏览器、显示提示与倒计时，取消立即调用 oauthCancel', async () => {
    h.api.oauthStart.mockResolvedValue(codeStart())
    h.api.oauthPoll.mockResolvedValue({ status: 'pending' })
    const w = mountPanel('claude')
    await flushPromises()
    await w.find('input[aria-label="账户标识"]').setValue('  工作账号  ')
    await btn(w, '开始授权').trigger('click')
    await flushPromises()

    expect(h.api.oauthStart).toHaveBeenCalledWith('claude', '工作账号')
    expect(h.openExternal).toHaveBeenCalledWith('https://claude.ai/oauth/authorize')
    expect(w.text()).toContain('已在浏览器中打开，请完成登录')
    expect(w.find('[data-testid="oauth-countdown"]').text()).toMatch(/剩余时间\s*0[45]:\d\d/)

    await w.find('[data-testid="oauth-reopen"]').trigger('click')
    expect(h.openExternal).toHaveBeenCalledTimes(2)

    await btn(w, '取消').trigger('click')
    expect(h.api.oauthCancel).toHaveBeenCalledWith('s-code')
    expect(w.text()).toContain('授权已取消')
    expect(w.emitted('cancel')).toBeTruthy()
    // 取消后不再轮询
    h.api.oauthPoll.mockClear()
    await vi.advanceTimersByTimeAsync(10_000)
    expect(h.api.oauthPoll).not.toHaveBeenCalled()
    await btn(w, '重新开始').trigger('click')
    expect(btn(w, '开始授权')).toBeTruthy()
  })

  it('自动打开浏览器失败时给出提示，但流程继续', async () => {
    h.openExternal.mockRejectedValue(new Error('boom'))
    h.api.oauthStart.mockResolvedValue(codeStart())
    h.api.oauthPoll.mockResolvedValue({ status: 'pending' })
    const w = mountPanel('claude')
    await flushPromises()
    await btn(w, '开始授权').trigger('click')
    await flushPromises()
    expect(w.text()).toContain('无法自动打开浏览器')
    expect(w.text()).toContain('已在浏览器中打开，请完成登录')
  })

  it('设备码流程：大号等宽设备码、复制、可点击的验证链接、倒计时', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true })
    h.api.oauthStart.mockResolvedValue(deviceStart())
    h.api.oauthPoll.mockResolvedValue({ status: 'pending' })
    const w = mountPanel('github')
    await flushPromises()
    await btn(w, '开始授权').trigger('click')
    await flushPromises()

    const code = userCodeEl(w)
    expect(code.text()).toBe('ABCD-1234')
    expect(code.element.tagName).toBe('CODE')
    expect(code.classes()).toContain('user-code')
    // 设备码流程不自动打开浏览器，由用户点击验证链接
    expect(h.openExternal).not.toHaveBeenCalled()

    await btn(w, '复制').trigger('click')
    await flushPromises()
    expect(writeText).toHaveBeenCalledWith('ABCD-1234')
    expect(w.text()).toContain('已复制')

    await w.find('[data-testid="oauth-verification-link"]').trigger('click')
    expect(h.openExternal).toHaveBeenCalledWith('https://github.com/login/device')
    expect(w.find('[data-testid="oauth-countdown"]').text()).toMatch(/剩余时间\s*0[45]:\d\d/)

    await vi.advanceTimersByTimeAsync(60_000)
    expect(w.find('[data-testid="oauth-countdown"]').text()).toMatch(/剩余时间\s*0[34]:\d\d/)
  })

  it('重新授权：调用 oauthReauthorize，而不是 oauthStart', async () => {
    const conn: Connection = {
      id: 'conn-1', providerId: 'claude', accountLabel: 'old@example.com', kind: 'oauth',
      status: 'needsReauth', priority: 1, weight: 1, enabled: true,
    }
    h.api.oauthReauthorize.mockResolvedValue(codeStart())
    h.api.oauthPoll.mockResolvedValue({ status: 'pending' })
    const w = mountPanel('claude', { reauthConnection: conn })
    await flushPromises()
    expect(w.find('input[aria-label="账户标识"]').exists()).toBe(false)
    await btn(w, '开始重新授权').trigger('click')
    await flushPromises()
    expect(h.api.oauthReauthorize).toHaveBeenCalledWith('conn-1')
    expect(h.api.oauthStart).not.toHaveBeenCalled()
  })
})

describe('OAuthPanel：轮询与终态', () => {
  async function startRunning(id = 'claude', start: OAuthStartResult = codeStart()) {
    h.api.oauthStart.mockResolvedValue(start)
    const w = mountPanel(id)
    await flushPromises()
    await btn(w, '开始授权').trigger('click')
    await flushPromises()
    return w
  }

  it('每 1500ms 轮询一次（setTimeout 链），成功后发出 done 并停止', async () => {
    h.api.oauthPoll
      .mockResolvedValueOnce({ status: 'pending' })
      .mockResolvedValueOnce({ status: 'pending' })
      .mockResolvedValueOnce({ status: 'succeeded', connectionId: 'conn-9' })
    const w = await startRunning()
    expect(h.api.oauthPoll).toHaveBeenCalledTimes(0)
    await vi.advanceTimersByTimeAsync(1500)
    expect(h.api.oauthPoll).toHaveBeenCalledTimes(1)
    expect(h.api.oauthPoll).toHaveBeenLastCalledWith('s-code')
    await vi.advanceTimersByTimeAsync(1500)
    expect(h.api.oauthPoll).toHaveBeenCalledTimes(2)
    await vi.advanceTimersByTimeAsync(1500)
    expect(h.api.oauthPoll).toHaveBeenCalledTimes(3)

    expect(w.emitted('done')).toEqual([['conn-9']])
    expect(w.text()).toContain('授权成功')
    await vi.advanceTimersByTimeAsync(10_000)
    expect(h.api.oauthPoll).toHaveBeenCalledTimes(3)
    expect(h.api.oauthCancel).not.toHaveBeenCalled()
  })

  it('轮询请求不会重叠：上一次没返回就不会发下一次', async () => {
    let release!: (r: OAuthPollResult) => void
    h.api.oauthPoll.mockReturnValue(new Promise<OAuthPollResult>((r) => (release = r)))
    await startRunning()
    await vi.advanceTimersByTimeAsync(15_000)
    expect(h.api.oauthPoll).toHaveBeenCalledTimes(1)
    h.api.oauthPoll.mockResolvedValue({ status: 'pending' })
    release({ status: 'pending' })
    await vi.advanceTimersByTimeAsync(1500)
    expect(h.api.oauthPoll).toHaveBeenCalledTimes(2)
  })

  it('failed(access_denied)：显示映射后的中文，不出现原始字符串，可重试', async () => {
    h.api.oauthPoll.mockResolvedValue({ status: 'failed', code: 'access_denied' })
    const w = await startRunning()
    await vi.advanceTimersByTimeAsync(1500)
    expect(w.find('[role="alert"]').text()).toBe(oauthErrorText('access_denied'))
    expect(w.html()).not.toContain('access_denied')
    expect(w.emitted('done')).toBeUndefined()

    h.api.oauthPoll.mockResolvedValue({ status: 'pending' })
    await btn(w, '重试').trigger('click')
    await flushPromises()
    expect(h.api.oauthStart).toHaveBeenCalledTimes(2)
    expect(w.text()).toContain('已在浏览器中打开，请完成登录')
  })

  it('未知错误码使用通用文案，不显示服务端原始文本', async () => {
    h.api.oauthPoll.mockRejectedValue({ code: 'weird_code', message: 'RAW SERVER TEXT with token=abc' })
    const w = await startRunning()
    await vi.advanceTimersByTimeAsync(1500)
    expect(w.find('[role="alert"]').text()).toBe(oauthErrorText('weird_code'))
    expect(w.html()).not.toContain('RAW SERVER TEXT')
    expect(w.html()).not.toContain('token=abc')
  })

  it('start 抛出非 OAuthApiError 时也只显示通用文案', async () => {
    h.api.oauthStart.mockRejectedValue(new Error('RAW boom with secret'))
    const w = mountPanel('claude')
    await flushPromises()
    await btn(w, '开始授权').trigger('click')
    await flushPromises()
    expect(w.find('[role="alert"]').text()).toBe(oauthErrorText('internal'))
    expect(w.html()).not.toContain('RAW boom')
  })

  it('poll 返回 expired：显示已过期并可重新开始', async () => {
    h.api.oauthPoll.mockResolvedValue({ status: 'expired' })
    const w = await startRunning()
    await vi.advanceTimersByTimeAsync(1500)
    expect(w.text()).toContain('授权已过期')
    await btn(w, '重新开始').trigger('click')
    expect(btn(w, '开始授权')).toBeTruthy()
  })

  it('poll 返回 cancelled：显示已取消', async () => {
    h.api.oauthPoll.mockResolvedValue({ status: 'cancelled' })
    const w = await startRunning()
    await vi.advanceTimersByTimeAsync(1500)
    expect(w.text()).toContain('授权已取消')
    expect(btn(w, '重新开始')).toBeTruthy()
  })

  it('倒计时到期：停止轮询、显示已过期，并尽力取消会话', async () => {
    h.api.oauthPoll.mockResolvedValue({ status: 'pending' })
    const w = await startRunning('claude', codeStart({ expiresAt: new Date(Date.now() + 4000).toISOString() }))
    await vi.advanceTimersByTimeAsync(6000)
    expect(w.text()).toContain('授权已过期')
    expect(h.api.oauthCancel).toHaveBeenCalledWith('s-code')
    const calls = h.api.oauthPoll.mock.calls.length
    await vi.advanceTimersByTimeAsync(20_000)
    expect(h.api.oauthPoll).toHaveBeenCalledTimes(calls)
  })

  it('卸载：停止轮询并尽力取消会话（取消失败也不抛错）', async () => {
    h.api.oauthPoll.mockResolvedValue({ status: 'pending' })
    h.api.oauthCancel.mockRejectedValue(new Error('already gone'))
    const w = await startRunning()
    await vi.advanceTimersByTimeAsync(3000)
    const calls = h.api.oauthPoll.mock.calls.length
    expect(calls).toBeGreaterThan(0)
    w.unmount()
    expect(h.api.oauthCancel).toHaveBeenCalledWith('s-code')
    await vi.advanceTimersByTimeAsync(30_000)
    expect(h.api.oauthPoll).toHaveBeenCalledTimes(calls)
  })

  it('已经结束的会话在卸载时不再取消', async () => {
    h.api.oauthPoll.mockResolvedValue({ status: 'succeeded', connectionId: 'c1' })
    const w = await startRunning()
    await vi.advanceTimersByTimeAsync(1500)
    w.unmount()
    expect(h.api.oauthCancel).not.toHaveBeenCalled()
  })

  it('start 尚未返回时卸载：返回后立即取消新建的会话', async () => {
    let release!: (s: OAuthStartResult) => void
    h.api.oauthStart.mockReturnValue(new Promise<OAuthStartResult>((r) => (release = r)))
    const w = mountPanel('claude')
    await flushPromises()
    await btn(w, '开始授权').trigger('click')
    w.unmount()
    release(codeStart({ sessionId: 's-late' }))
    await flushPromises()
    expect(h.api.oauthCancel).toHaveBeenCalledWith('s-late')
    expect(h.openExternal).not.toHaveBeenCalled()
    expect(h.api.oauthPoll).not.toHaveBeenCalled()
  })
})
