import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { ApiKeyInfo, AppSettings, ClientImportPreview, SystemStatus } from '@/types'

const SECRET = 'sk-r-FULLSECRET-0123456789'

const h = vi.hoisted(() => ({
  api: {
    listApiKeys: vi.fn(),
    createApiKey: vi.fn(),
    getSettings: vi.fn(),
    listGatewayModels: vi.fn(),
    previewClientImport: vi.fn(),
    applyClientImport: vi.fn(),
  },
  openCcSwitchLink: vi.fn(),
}))
vi.mock('@/api', () => ({ api: h.api }))
vi.mock('@/api/openExternal', () => ({ openCcSwitchLink: h.openCcSwitchLink, openExternal: vi.fn() }))

import ConnectPanel from './ConnectPanel.vue'
import { useApiKeysStore } from '@/stores/apiKeys'
import { useSystemStore } from '@/stores/system'

const ALL = ['openai-chat', 'openai-responses', 'anthropic-messages', 'gemini'] as const

function keyInfo(over: Partial<ApiKeyInfo> = {}): ApiKeyInfo {
  return {
    id: 'k1', name: 'Cursor', masked: 'sk-r…6789', secret: SECRET, protocols: [...ALL], allowedModels: [], rpmLimit: null,
    tpmLimit: null, totalLimit: null, callCount: 0, createdAt: '2026-10-01T08:00:00Z', lastUsedAt: null, expiresAt: null, disabled: false, ...over,
  }
}
const settings = (over: Partial<AppSettings> = {}): AppSettings => ({
  listenAddress: '127.0.0.1', port: 20128, autoStart: false, trayEnabled: true, timeoutSeconds: 120, retryCount: 2,
  language: 'zh-CN', theme: 'light', fontScale: 1, redactLogs: true, ...over,
})
const status = (running: boolean, address = '127.0.0.1', port = 20128): SystemStatus => ({
  gateway: { running, address, port, error: null }, apiRequests: 0, successRate: null, avgLatencyMs: null,
})
const preview = (over: Partial<ClientImportPreview> = {}): ClientImportPreview => ({
  client: 'codex', path: '/Users/me/.codex/config.toml', exists: true, unchanged: false, error: null,
  changes: [
    { field: 'model', before: 'old-model', after: 'openai/gpt-4o-mini' },
    { field: 'model_providers.router.experimental_bearer_token', before: null, after: SECRET },
    { field: 'model_providers.router.env_key', before: 'ROUTER_API_KEY', after: null },
  ],
  ...over,
})

const q = (sel: string) => document.body.querySelector<HTMLElement>(sel)
const text = (sel: string) => q(sel)?.textContent ?? ''
let wrapper: VueWrapper
let clipboard: ReturnType<typeof vi.fn>

async function mountPanel(opts: { keys?: ApiKeyInfo[]; set?: AppSettings; models?: { id: string; providerId: string; providerName: string }[] } = {}) {
  h.api.listApiKeys.mockResolvedValue(opts.keys ?? [keyInfo()])
  h.api.getSettings.mockResolvedValue(opts.set ?? settings())
  h.api.listGatewayModels.mockResolvedValue(opts.models ?? [{ id: 'openai/gpt-4o-mini', providerId: 'openai', providerName: 'OpenAI' }])
  await useApiKeysStore().load()
  wrapper = mount(ConnectPanel, { attachTo: document.body })
  useSystemStore().status = status(true)
  await flushPromises()
}
async function pick(sel: string, value: string) {
  const el = q(sel) as HTMLSelectElement | HTMLInputElement
  el.value = value
  el.dispatchEvent(new Event(el.tagName === 'SELECT' ? 'change' : 'input'))
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
  h.openCcSwitchLink.mockReset()
  h.api.previewClientImport.mockResolvedValue(preview())
  clipboard = vi.fn().mockResolvedValue(undefined)
  Object.defineProperty(navigator, 'clipboard', { value: { writeText: clipboard }, configurable: true })
})
afterEach(() => {
  wrapper?.unmount()
  document.body.innerHTML = ''
})

describe('ConnectPanel · 连接参数', () => {
  it('密钥以明文展示并可复制，默认选好第一个模型', async () => {
    await mountPanel()
    expect(text('[data-testid="key-full"]')).toBe(SECRET)
    expect((q('[data-testid="model-input"]') as HTMLInputElement).value).toBe('openai/gpt-4o-mini')
    q('.keyline button')!.click()
    await flushPromises()
    expect(clipboard).toHaveBeenCalledWith(SECRET)
  })

  it('多个密钥时默认选放行全部协议的那个；停用、过期、没存明文的不出现', async () => {
    await mountPanel({
      keys: [
        keyInfo({ id: 'narrow', name: 'Narrow', protocols: ['openai-chat'], secret: 'sk-narrow' }),
        keyInfo({ id: 'full', name: 'Full', secret: 'sk-full' }),
        keyInfo({ id: 'off', name: 'Off', disabled: true }),
        keyInfo({ id: 'old', name: 'Old', expiresAt: '2020-01-01T00:00:00Z' }),
        keyInfo({ id: 'legacy', name: 'Legacy', secret: undefined }),
      ],
    })
    const select = q('[data-testid="key-select"]') as HTMLSelectElement
    expect([...select.options].map((o) => o.value)).toEqual(['narrow', 'full'])
    expect(select.value).toBe('full')
    expect(text('[data-testid="key-full"]')).toBe('sk-full')
    await pick('[data-testid="key-select"]', 'narrow')
    expect(text('[data-testid="key-full"]')).toBe('sk-narrow')
  })

  it('没有密钥：一键创建，创建后立刻可用', async () => {
    await mountPanel({ keys: [] })
    expect(text('[data-testid="no-key"]')).toContain('还没有可用的 API 密钥')
    expect((q('[data-testid="open-codex"]') as HTMLButtonElement).disabled).toBe(true)

    h.api.createApiKey.mockResolvedValue({ key: keyInfo({ id: 'new', name: 'FreeApiBox 接入' }), secret: SECRET })
    await click('[data-testid="create-key"]')
    expect(h.api.createApiKey).toHaveBeenCalledWith({
      name: 'FreeApiBox 接入', expiresAt: null, protocols: [...ALL], allowedModels: [], rpmLimit: null, tpmLimit: null, totalLimit: null,
    })
    expect(q('[data-testid="no-key"]')).toBeNull()
    expect(text('[data-testid="key-full"]')).toBe(SECRET)
    expect((q('[data-testid="open-codex"]') as HTMLButtonElement).disabled).toBe(false)
  })

  it('只有旧密钥（没存明文）时给出对应说明；创建失败显示原因', async () => {
    await mountPanel({ keys: [keyInfo({ secret: undefined })] })
    expect(text('[data-testid="no-key"]')).toContain('没有保存明文')
    h.api.createApiKey.mockRejectedValue('数据库忙')
    await click('[data-testid="create-key"]')
    expect(text('[data-testid="create-error"]')).toContain('数据库忙')
  })

  it('没有模型时按钮不可用并提示；手动输入后可用', async () => {
    await mountPanel({ models: [] })
    expect(q('[data-testid="no-models"]')).not.toBeNull()
    expect((q('[data-testid="open-claude"]') as HTMLButtonElement).disabled).toBe(true)
    expect(q('[data-testid="import-disabled-hint"]')).not.toBeNull()
    await pick('[data-testid="model-input"]', 'deepseek-chat')
    expect((q('[data-testid="open-claude"]') as HTMLButtonElement).disabled).toBe(false)
  })

  it('读取模型列表失败时显示原因', async () => {
    await mountPanel()
    h.api.listGatewayModels.mockRejectedValue('网关异常')
    wrapper.unmount()
    wrapper = mount(ConnectPanel, { attachTo: document.body })
    await flushPromises()
    expect(document.body.textContent).toContain('读取模型列表失败：网关异常')
  })

  it('导入用的地址：通配监听地址换成 127.0.0.1', async () => {
    await mountPanel({ set: settings({ listenAddress: '0.0.0.0', port: 9000 }) })
    useSystemStore().status = status(false)
    await flushPromises()
    await click('[data-testid="open-codex"]')
    expect(h.api.previewClientImport).toHaveBeenCalledWith(expect.objectContaining({ origin: 'http://127.0.0.1:9000' }))
  })
})

describe('ConnectPanel · Codex / Claude Code 弹窗', () => {
  it('点按钮出弹窗，展示目标文件、字段变化和说明；此时还没有写入任何东西', async () => {
    await mountPanel()
    await click('[data-testid="open-codex"]')
    expect(h.api.previewClientImport).toHaveBeenCalledWith({ client: 'codex', origin: 'http://127.0.0.1:20128', apiKey: SECRET, model: 'openai/gpt-4o-mini' })
    expect(text('[data-testid="import-path"]')).toBe('/Users/me/.codex/config.toml')
    expect(text('[data-testid="file-state"]')).toContain('修改现有文件')
    const rows = [...document.body.querySelectorAll('[data-testid="changes"] tbody tr')].map((r) =>
      [...r.querySelectorAll('td')].map((td) => td.textContent!.trim()),
    )
    expect(rows).toEqual([
      ['model', 'old-model', 'openai/gpt-4o-mini'],
      ['model_providers.router.experimental_bearer_token', '（无）', SECRET],
      ['model_providers.router.env_key', 'ROUTER_API_KEY', '（删除）'],
    ])
    const modal = text('[data-testid="import-modal-codex"]')
    expect(modal).toContain('自动备份')
    expect(modal).toContain('切换为 router')
    expect(modal).toContain('重启 Codex')
    expect(h.api.applyClientImport).not.toHaveBeenCalled()
  })

  it('确认后写入，显示结果与备份路径', async () => {
    await mountPanel()
    await click('[data-testid="open-codex"]')
    h.api.applyClientImport.mockResolvedValue({ path: '/Users/me/.codex/config.toml', backupPath: '/Users/me/.codex/config.toml.router-bak-20261015-103000', changed: true })
    await click('[data-testid="confirm-import"]')
    expect(h.api.applyClientImport).toHaveBeenCalledWith({ client: 'codex', origin: 'http://127.0.0.1:20128', apiKey: SECRET, model: 'openai/gpt-4o-mini' })
    expect(text('[data-testid="import-done"]')).toContain('已导入到 Codex')
    expect(text('[data-testid="backup-path"]')).toContain('router-bak-20261015-103000')
    await click('[data-testid="done"]')
    expect(q('[data-testid="import-modal-codex"]')).toBeNull()
  })

  it('取消不会写入', async () => {
    await mountPanel()
    await click('[data-testid="open-claude"]')
    expect(h.api.previewClientImport).toHaveBeenCalledWith(expect.objectContaining({ client: 'claude' }))
    ;[...document.body.querySelectorAll('button')].find((b) => b.textContent?.trim() === '取消')!.click()
    await flushPromises()
    expect(h.api.applyClientImport).not.toHaveBeenCalled()
    expect(q('[data-testid="import-modal-claude"]')).toBeNull()
  })

  it('现有文件无法解析：显示原因并禁用确认', async () => {
    h.api.previewClientImport.mockResolvedValue(preview({ changes: [], error: '现有 config.toml 不是合法的 TOML（第 3 行），为避免覆盖已拒绝合并。' }))
    await mountPanel()
    await click('[data-testid="open-codex"]')
    expect(text('[data-testid="preview-error"]')).toContain('已拒绝合并')
    expect(q('[data-testid="changes"]')).toBeNull()
    expect((q('[data-testid="confirm-import"]') as HTMLButtonElement).disabled).toBe(true)
  })

  it('配置已是最新：提示无需导入，没有确认按钮', async () => {
    h.api.previewClientImport.mockResolvedValue(preview({ unchanged: true }))
    await mountPanel()
    await click('[data-testid="open-codex"]')
    expect(text('[data-testid="file-state"]')).toContain('已是最新')
    expect(q('[data-testid="unchanged-note"]')).not.toBeNull()
    expect(q('[data-testid="confirm-import"]')).toBeNull()
  })

  it('写入失败：显示后端错误，可以重试', async () => {
    await mountPanel()
    await click('[data-testid="open-claude"]')
    h.api.applyClientImport.mockRejectedValueOnce('写入 /Users/me/.claude/settings.json 失败：Permission denied')
    await click('[data-testid="confirm-import"]')
    expect(text('[data-testid="import-failure"]')).toContain('Permission denied')
    expect(q('[data-testid="import-done"]')).toBeNull()
    expect((q('[data-testid="confirm-import"]') as HTMLButtonElement).disabled).toBe(false)
  })

  it('读取预览本身失败也能看到原因', async () => {
    h.api.previewClientImport.mockRejectedValue('无法确定用户主目录')
    await mountPanel()
    await click('[data-testid="open-codex"]')
    expect(text('[data-testid="import-failure"]')).toContain('无法确定用户主目录')
    expect((q('[data-testid="confirm-import"]') as HTMLButtonElement).disabled).toBe(true)
  })
})

describe('ConnectPanel · CC Switch 弹窗', () => {
  it('展示将导入的信息；确认前不会唤起 CC Switch', async () => {
    await mountPanel()
    await click('[data-testid="open-ccswitch"]')
    const facts = text('[data-testid="cc-facts"]')
    expect(facts).toContain('FreeApiBox')
    expect(facts).toContain('http://127.0.0.1:20128')
    expect(facts).toContain('openai/gpt-4o-mini')
    expect(facts).toContain(SECRET)
    expect(h.openCcSwitchLink).not.toHaveBeenCalled()
  })

  it('默认导入为 Claude Code；切到 Codex 后接口地址带 /v1，确认时用对应链接', async () => {
    await mountPanel()
    await click('[data-testid="open-ccswitch"]')
    h.openCcSwitchLink.mockResolvedValue(undefined)
    await click('[data-testid="confirm-import"]')
    const claude = new URL(h.openCcSwitchLink.mock.calls[0][0] as string)
    expect(claude.searchParams.get('app')).toBe('claude')
    expect(claude.searchParams.get('endpoint')).toBe('http://127.0.0.1:20128')
    expect(claude.searchParams.get('apiKey')).toBe(SECRET)
    expect(text('[data-testid="cc-sent"]')).toContain('Claude Code')

    await click('[data-testid="cc-done"]')
    await click('[data-testid="open-ccswitch"]')
    await click('[data-testid="tab-codex"]')
    expect(text('[data-testid="cc-facts"]')).toContain('http://127.0.0.1:20128/v1')
    await click('[data-testid="confirm-import"]')
    const codex = new URL(h.openCcSwitchLink.mock.calls[1][0] as string)
    expect(codex.searchParams.get('app')).toBe('codex')
    expect(codex.searchParams.get('endpoint')).toBe('http://127.0.0.1:20128/v1')
  })

  it('唤起失败：给出可操作的提示，仍可重试', async () => {
    await mountPanel()
    await click('[data-testid="open-ccswitch"]')
    h.openCcSwitchLink.mockRejectedValue(new Error('no handler'))
    await click('[data-testid="confirm-import"]')
    expect(text('[data-testid="cc-failure"]')).toContain('请确认已安装 CC Switch')
    expect(q('[data-testid="cc-sent"]')).toBeNull()
    expect((q('[data-testid="confirm-import"]') as HTMLButtonElement).disabled).toBe(false)
  })
})
