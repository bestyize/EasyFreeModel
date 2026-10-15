import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { ApiKeyInfo, AppSettings } from '@/types'

const SECRET = 'sk-r-SUPERSECRET-0123456789abcdef'

const h = vi.hoisted(() => ({
  api: {
    listApiKeys: vi.fn(),
    createApiKey: vi.fn(),
    rotateApiKey: vi.fn(),
    setApiKeyDisabled: vi.fn(),
    deleteApiKey: vi.fn(),
    getSettings: vi.fn(),
    listGatewayModels: vi.fn(),
    previewClientImport: vi.fn(),
    applyClientImport: vi.fn(),
  },
}))
vi.mock('@/api', () => ({ api: h.api }))

import ApiKeysView from '@/views/ApiKeysView.vue'

function keyInfo(over: Partial<ApiKeyInfo> = {}): ApiKeyInfo {
  return {
    id: 'k1', name: 'Cursor', masked: 'sk-r…a9f2', protocols: ['openai-chat'], allowedModels: [],
    rpmLimit: null, tpmLimit: null, totalLimit: null, callCount: 3, createdAt: '2026-10-01T08:00:00Z',
    lastUsedAt: null, expiresAt: null, disabled: false, ...over,
  }
}
function settings(listenAddress: string): AppSettings {
  return {
    listenAddress, port: 20128, autoStart: false, trayEnabled: true, timeoutSeconds: 120, retryCount: 2,
    language: 'zh-CN', theme: 'light', fontScale: 1, redactLogs: true,
  }
}

const body = () => document.body
const q = (sel: string) => document.body.querySelector<HTMLElement>(sel)
let wrapper: VueWrapper

async function mountView(listenAddress = '127.0.0.1', keys: ApiKeyInfo[] = [keyInfo()]) {
  h.api.listApiKeys.mockResolvedValue(keys)
  h.api.getSettings.mockResolvedValue(settings(listenAddress))
  wrapper = mount(ApiKeysView, { attachTo: document.body })
  await flushPromises()
}

beforeEach(() => {
  setActivePinia(createPinia())
  Object.values(h.api).forEach((f) => f.mockReset())
  h.api.listGatewayModels.mockResolvedValue([])
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: vi.fn().mockResolvedValue(undefined) } })
})
afterEach(() => {
  wrapper?.unmount()
  document.body.innerHTML = ''
})

describe('ApiKeysView', () => {
  it('每把可用密钥都可在密钥旁打开客户端列表并选择模型', async () => {
    h.api.listGatewayModels.mockResolvedValue([{ id: 'deepseek-chat', providerId: 'deepseek', providerName: 'DeepSeek' }])
    await mountView('127.0.0.1', [keyInfo({ id: 'k1', secret: SECRET, protocols: ['anthropic-messages'] })])
    expect(wrapper.get('[data-testid="import-k1"]').element.closest('.key-card')).not.toBeNull()
    expect(wrapper.get('[data-testid="import-k1"]').text()).toContain('一键导入')
    await wrapper.get('[data-testid="import-k1"]').trigger('click')
    await flushPromises()
    expect(q('[data-testid="import-target-list"]')?.textContent).toContain('Claude Desktop')
    q('[data-testid="pick-claude"]')!.click()
    await flushPromises()
    expect((q('#import-default-model') as HTMLInputElement).value).toBe('deepseek-chat')
    expect((q('#import-haiku') as HTMLInputElement).placeholder).toBe('沿用默认模型')
    expect((q('#import-haiku') as HTMLInputElement).value).toBe('')
    expect((q('#import-sonnet') as HTMLInputElement).value).toBe('')
    expect((q('#import-opus') as HTMLInputElement).value).toBe('')
    expect(h.api.previewClientImport).toHaveBeenCalledWith(expect.objectContaining({
      client: 'claude', apiKey: SECRET, model: 'deepseek-chat',
      haikuModel: '', sonnetModel: '', opusModel: '',
    }))
  })

  it('Claude Code 按所选档位预览并写入', async () => {
    h.api.listGatewayModels.mockResolvedValue([
      { id: 'fast', providerId: 'a', providerName: 'A' },
      { id: 'strong', providerId: 'a', providerName: 'A' },
    ])
    h.api.previewClientImport.mockImplementation(async () => ({
      client: 'claude', path: '~/.claude/settings.json', exists: true, unchanged: false, error: null,
      changes: [{ field: 'env.ANTHROPIC_MODEL', before: 'old', after: 'fast' }],
    }))
    h.api.applyClientImport.mockResolvedValue({ path: '~/.claude/settings.json', backupPath: 'settings.json.bak', changed: true })
    await mountView('127.0.0.1', [keyInfo({ secret: SECRET, protocols: ['anthropic-messages'] })])
    await wrapper.get('[data-testid="import-k1"]').trigger('click')
    q('[data-testid="pick-claude"]')!.click()
    await flushPromises()
    const opus = q('#import-opus') as HTMLInputElement
    opus.value = 'strong'
    opus.dispatchEvent(new Event('input'))
    await flushPromises()
    expect(h.api.previewClientImport).toHaveBeenLastCalledWith(expect.objectContaining({ opusModel: 'strong', apiKey: SECRET }))
    q('[data-testid="confirm-import"]')!.click()
    await flushPromises()
    expect(h.api.applyClientImport).toHaveBeenCalledWith(expect.objectContaining({ opusModel: 'strong' }))
    expect(q('[role="status"]')?.textContent).toContain('配置已写入')
  })

  it('Claude Desktop 可配置并复制多个模型，不写入 Claude Code 配置', async () => {
    h.api.listGatewayModels.mockResolvedValue([
      { id: 'first', providerId: 'a', providerName: 'A' },
      { id: 'second', providerId: 'a', providerName: 'A' },
    ])
    await mountView('127.0.0.1', [keyInfo({ secret: SECRET, protocols: ['openai-chat'] })])
    await wrapper.get('[data-testid="import-k1"]').trigger('click')
    q('[data-testid="pick-desktop"]')!.click()
    await flushPromises()
    expect(q('[data-testid="setup-desktop"]')?.textContent).toContain('模型列表')
    q('[data-testid="add-desktop-model"]')!.click()
    await flushPromises()
    const extra = q('#import-desktop-model-0') as HTMLInputElement
    extra.value = 'second'
    extra.dispatchEvent(new Event('input'))
    await flushPromises()
    expect(q('[data-testid="desktop-copy-model-0"]')?.textContent).toContain('first')
    expect(q('[data-testid="desktop-copy-model-1"]')?.textContent).toContain('second')
    q('[data-testid="copy-desktop-models"]')!.click()
    await flushPromises()
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith('first\nsecond')
    expect(h.api.previewClientImport).not.toHaveBeenCalled()
  })

  it('停用或已过期的密钥不可导入', async () => {
    await mountView('127.0.0.1', [
      keyInfo({ id: 'disabled', disabled: true, secret: SECRET }),
      keyInfo({ id: 'expired', expiresAt: '2020-01-01T00:00:00Z', secret: SECRET }),
    ])
    expect((wrapper.get('[data-testid="import-disabled"]').element as HTMLButtonElement).disabled).toBe(true)
    expect((wrapper.get('[data-testid="import-expired"]').element as HTMLButtonElement).disabled).toBe(true)
  })

  it('列表显示掩码密钥与状态，完整密钥不出现', async () => {
    await mountView()
    expect(wrapper.text()).toContain('sk-r…a9f2')
    expect(wrapper.text()).toContain('启用中')
    expect(body().textContent).not.toContain(SECRET)
  })

  it('创建成功后展示完整密钥，可关闭并提示可在列表中查看', async () => {
    await mountView()
    h.api.createApiKey.mockResolvedValue({ key: keyInfo({ id: 'k2', name: 'New', masked: 'sk-r…cdef' }), secret: SECRET })

    await wrapper.get('[data-testid="open-create"]').trigger('click')
    const name = q('#ck-name') as HTMLInputElement
    name.value = 'New'
    name.dispatchEvent(new Event('input'))
    q('#create-key-form')!.dispatchEvent(new Event('submit', { cancelable: true }))
    await flushPromises()

    expect(h.api.createApiKey).toHaveBeenCalledTimes(1)
    expect(q('[data-testid="secret-value"]')?.textContent).toBe(SECRET)
    expect(body().textContent).toContain('完整密钥已保存在本地数据库中')

    q('[data-testid="close-secret"]')!.click()
    await flushPromises()

    expect(q('[data-testid="secret-value"]')).toBeNull()
  })

  it('列表中的密钥只有一个显示按钮，可切换明文和掩码', async () => {
    await mountView('127.0.0.1', [keyInfo({ id: 'k1', masked: 'sk-r…a9f2', secret: SECRET })])
    // 初始显示掩码
    expect(wrapper.get('[data-testid="key-value-k1"]').text()).toBe('sk-r…a9f2')

    // 点击小眼睛显示完整密钥
    await wrapper.get('[data-testid="toggle-plain-k1"]').trigger('click')
    expect(wrapper.get('[data-testid="key-value-k1"]').text()).toBe(SECRET)

    // 再次点击隐藏
    await wrapper.get('[data-testid="toggle-plain-k1"]').trigger('click')
    expect(wrapper.get('[data-testid="key-value-k1"]').text()).toBe('sk-r…a9f2')

    expect(wrapper.find('[data-testid="view-k1"]').exists()).toBe(false)
  })

  it('创建时限额非法则不提交并提示', async () => {
    await mountView()
    await wrapper.get('[data-testid="open-create"]').trigger('click')
    const set = (sel: string, v: string) => {
      const el = q(sel) as HTMLInputElement
      el.value = v
      el.dispatchEvent(new Event('input'))
    }
    set('#ck-name', 'X')
    set('#ck-rpm', '-3')
    set('#ck-tpm', '1.5')
    q('#create-key-form')!.dispatchEvent(new Event('submit', { cancelable: true }))
    await flushPromises()
    expect(h.api.createApiKey).not.toHaveBeenCalled()
    expect(body().textContent).toContain('RPM必须是正整数')
    expect(body().textContent).toContain('TPM必须是正整数')
  })

  it('创建失败时显示错误且不展示密钥', async () => {
    await mountView()
    h.api.createApiKey.mockRejectedValue(new Error('磁盘已满'))
    await wrapper.get('[data-testid="open-create"]').trigger('click')
    const name = q('#ck-name') as HTMLInputElement
    name.value = 'X'
    name.dispatchEvent(new Event('input'))
    q('#create-key-form')!.dispatchEvent(new Event('submit', { cancelable: true }))
    await flushPromises()
    expect(body().textContent).toContain('创建失败：磁盘已满')
    expect(q('[data-testid="secret-value"]')).toBeNull()
  })

  it('删除需要二次确认，取消不调用接口，确认后才删除', async () => {
    await mountView()
    h.api.deleteApiKey.mockResolvedValue(undefined)
    await wrapper.get('[data-testid="delete-k1"]').trigger('click')
    expect(h.api.deleteApiKey).not.toHaveBeenCalled()
    expect(body().textContent).toContain('删除密钥')

    const buttons = () => Array.from(document.querySelectorAll<HTMLButtonElement>('[role="dialog"] button'))
    buttons().find((b) => b.textContent === '取消')!.click()
    await flushPromises()
    expect(h.api.deleteApiKey).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('Cursor')

    await wrapper.get('[data-testid="delete-k1"]').trigger('click')
    buttons().find((b) => b.textContent === '确认删除')!.click()
    await flushPromises()
    expect(h.api.deleteApiKey).toHaveBeenCalledWith('k1')
    expect(wrapper.text()).not.toContain('sk-r…a9f2')
  })

  it('轮换需要二次确认，确认后展示新密钥一次', async () => {
    await mountView()
    h.api.rotateApiKey.mockResolvedValue({ key: keyInfo({ masked: 'sk-r…cdef' }), secret: SECRET })
    await wrapper.get('[data-testid="rotate-k1"]').trigger('click')
    expect(h.api.rotateApiKey).not.toHaveBeenCalled()
    Array.from(document.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')).find((b) => b.textContent === '确认轮换')!.click()
    await flushPromises()
    expect(h.api.rotateApiKey).toHaveBeenCalledWith('k1')
    expect(q('[data-testid="secret-value"]')?.textContent).toBe(SECRET)
    q('[data-testid="close-secret"]')!.click()
    await flushPromises()
    expect(body().textContent).not.toContain(SECRET)
  })

  it('停用无需确认，直接调用并更新状态；失败显示错误', async () => {
    await mountView()
    h.api.setApiKeyDisabled.mockResolvedValueOnce(keyInfo({ disabled: true }))
    await wrapper.get('[data-testid="toggle-k1"]').trigger('click')
    await flushPromises()
    expect(h.api.setApiKeyDisabled).toHaveBeenCalledWith('k1', true)
    expect(wrapper.text()).toContain('已停用')

    h.api.setApiKeyDisabled.mockRejectedValueOnce(new Error('后端错误'))
    await wrapper.get('[data-testid="toggle-k1"]').trigger('click')
    await flushPromises()
    expect(wrapper.get('[data-testid="action-error"]').text()).toContain('后端错误')
  })

  it.each(['0.0.0.0', '192.168.1.5'])('监听地址 %s 非回环时显示风险提醒', async (addr) => {
    await mountView(addr)
    expect(wrapper.find('[data-testid="risk-banner"]').exists()).toBe(true)
  })

  it.each(['127.0.0.1', 'localhost'])('监听地址 %s 时不显示风险提醒', async (addr) => {
    await mountView(addr)
    expect(wrapper.find('[data-testid="risk-banner"]').exists()).toBe(false)
  })
})
