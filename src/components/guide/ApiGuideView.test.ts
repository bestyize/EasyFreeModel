import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { ApiKeyInfo, AppSettings, SystemStatus } from '@/types'

const SECRET = 'sk-r-FULLSECRET-0123456789'

const h = vi.hoisted(() => ({
  api: {
    listApiKeys: vi.fn(),
    getSettings: vi.fn(),
    listGatewayModels: vi.fn(),
    systemStatus: vi.fn(),
    startGateway: vi.fn(),
    stopGateway: vi.fn(),
  },
}))
vi.mock('@/api', () => ({ api: h.api }))

import ApiGuideView from '@/views/ApiGuideView.vue'
import { useConnectStore } from '@/stores/connect'
import { useSystemStore } from '@/stores/system'

function keyInfo(over: Partial<ApiKeyInfo> = {}): ApiKeyInfo {
  return {
    id: 'k1', name: 'Cursor', masked: 'sk-r…6789', secret: SECRET,
    protocols: ['openai-chat', 'openai-responses', 'anthropic-messages', 'gemini'], allowedModels: [], rpmLimit: null,
    tpmLimit: null, totalLimit: null, callCount: 0, createdAt: '2026-10-01T08:00:00Z', lastUsedAt: null, expiresAt: null, disabled: false, ...over,
  }
}
const settings = (over: Partial<AppSettings> = {}): AppSettings => ({
  listenAddress: '127.0.0.1', port: 20128, autoStart: false, trayEnabled: true, timeoutSeconds: 120, retryCount: 2,
  language: 'zh-CN', theme: 'light', fontScale: 1, redactLogs: true, ...over,
})
const status = (running: boolean, address = '127.0.0.1', port = 20128, error: string | null = null): SystemStatus => ({
  gateway: { running, address, port, error }, apiRequests: 0, successRate: null, avgLatencyMs: null,
})

const q = (sel: string) => document.body.querySelector<HTMLElement>(sel)
const text = (sel: string) => q(sel)?.textContent ?? ''
let wrapper: VueWrapper

async function mountView(opts: { keys?: ApiKeyInfo[]; set?: AppSettings; running?: boolean; models?: { id: string; providerId: string; providerName: string }[] } = {}) {
  h.api.listApiKeys.mockResolvedValue(opts.keys ?? [keyInfo()])
  h.api.getSettings.mockResolvedValue(opts.set ?? settings())
  h.api.listGatewayModels.mockResolvedValue(opts.models ?? [{ id: 'openai/gpt-4o-mini', providerId: 'openai', providerName: 'OpenAI' }])
  wrapper = mount(ApiGuideView, {
    attachTo: document.body,
    global: { stubs: { RouterLink: { props: ['to'], template: '<a :href="to"><slot /></a>' } } },
  })
  useSystemStore().status = status(opts.running ?? true)
  await flushPromises()
}
async function click(sel: string) {
  const el = q(sel)
  expect(el, `找不到 ${sel}`).not.toBeNull()
  el!.click()
  await flushPromises()
}

beforeEach(() => {
  setActivePinia(createPinia())
  Object.values(h.api).forEach((f) => f.mockReset())
})
afterEach(() => {
  wrapper?.unmount()
  document.body.innerHTML = ''
})

describe('ApiGuideView · 网关地址', () => {
  it('网关地址是页面最前面的模块，连接参数和一键接入不在这个页面', async () => {
    await mountView()
    const sections = [...document.body.querySelectorAll('section')]
    expect(sections.map((s) => s.querySelector('h2')?.textContent)).toEqual(['网关地址', '请求示例'])
    expect(q('#g-params')).toBeNull()
    expect(q('#g-import')).toBeNull()
    expect(q('[data-testid="open-codex"]')).toBeNull()
  })

  it('列出各协议地址与 Base URL', async () => {
    await mountView()
    expect(text('[data-testid="addr-openai-chat"]')).toContain('http://127.0.0.1:20128/v1/chat/completions')
    expect(text('[data-testid="addr-openai-chat"]')).toContain('http://127.0.0.1:20128/v1')
    expect(text('[data-testid="addr-anthropic-messages"]')).toContain('http://127.0.0.1:20128/v1/messages')
    expect(q('[data-testid="stopped-banner"]')).toBeNull()
  })

  it('运行中以实际监听地址为准；通配地址显示局域网提示', async () => {
    await mountView({ set: settings({ listenAddress: '0.0.0.0', port: 9000 }), running: false })
    expect(text('[data-testid="addr-models"]')).toContain('http://127.0.0.1:9000/v1/models')
    expect(q('[data-testid="wildcard-hint"]')).not.toBeNull()
    useSystemStore().status = status(true, '10.0.0.8', 9100)
    await flushPromises()
    expect(text('[data-testid="addr-models"]')).toContain('http://10.0.0.8:9100/v1/models')
  })

  it('网关未运行：提示放在地址模块里，可一键启动，失败显示后端错误', async () => {
    h.api.startGateway.mockResolvedValue({ running: false, address: '127.0.0.1', port: 20128, error: '端口 20128 已被占用' })
    h.api.systemStatus.mockResolvedValue(status(false, '127.0.0.1', 20128, '端口 20128 已被占用'))
    await mountView({ running: false })
    expect(q('section[aria-labelledby="g-addr"] [data-testid="stopped-banner"]')).not.toBeNull()
    await click('[data-testid="start-gateway"]')
    expect(h.api.startGateway).toHaveBeenCalled()
    expect(text('[data-testid="start-error"]')).toContain('已被占用')
  })
})

describe('ApiGuideView · 请求示例', () => {
  it('示例直接用「API 密钥」页选好的密钥和模型，并说明来源', async () => {
    await mountView()
    expect(text('[data-testid="example-code"]')).toContain(`Bearer ${SECRET}`)
    expect(text('[data-testid="example-code"]')).toContain('"model": "openai/gpt-4o-mini"')
    const link = q('[data-testid="example-source"] a')!
    expect(link.getAttribute('href')).toBe('/api-keys')
    expect(link.textContent).toBe('API 密钥')
  })

  it('在别处改了模型，示例跟着变', async () => {
    await mountView()
    useConnectStore().model = 'deepseek-chat'
    await flushPromises()
    expect(text('[data-testid="example-code"]')).toContain('"model": "deepseek-chat"')
  })

  it('没有可用密钥或模型时用占位符', async () => {
    await mountView({ keys: [], models: [] })
    const code = text('[data-testid="example-code"]')
    expect(code).toContain('Bearer YOUR_API_KEY')
    expect(code).toContain('YOUR_MODEL')
  })

  it('切换协议和语言；Gemini 只有 curl，切过去时语言自动回到 curl', async () => {
    await mountView()
    await click('[data-testid="tab-anthropic-messages"]')
    expect(text('[data-testid="example-code"]')).toContain('/v1/messages')
    expect(text('[data-testid="example-code"]')).toContain('x-api-key')
    await click('[data-testid="tab-python"]')
    expect(text('[data-testid="example-code"]')).toContain('anthropic.Anthropic(')
    await click('[data-testid="tab-openai-responses"]')
    expect(text('[data-testid="example-code"]')).toContain('client.responses.create(')
    await click('[data-testid="tab-node"]')
    await click('[data-testid="tab-gemini"]')
    expect(q('[data-testid="tab-python"]')).toBeNull()
    expect(text('[data-testid="example-code"]')).toContain('x-goog-api-key')
    expect(text('[data-testid="example-code"]')).toContain('curl ')
  })

  it('点复制放进剪贴板的是完整示例', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true })
    await mountView()
    q('[data-testid="example-code"] button[aria-label="复制"]')!.click()
    await flushPromises()
    expect(writeText.mock.calls[0][0]).toContain(`Bearer ${SECRET}`)
  })
})
