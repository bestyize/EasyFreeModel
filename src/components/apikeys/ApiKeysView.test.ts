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
})
afterEach(() => {
  wrapper?.unmount()
  document.body.innerHTML = ''
})

describe('ApiKeysView', () => {
  it('密钥表下面是连接参数和一键接入；选中的密钥和模型就是接入面板里用的', async () => {
    h.api.listGatewayModels.mockResolvedValue([{ id: 'deepseek-chat', providerId: 'deepseek', providerName: 'DeepSeek' }])
    await mountView('127.0.0.1', [keyInfo({ id: 'k1', secret: SECRET })])
    const headings = [...wrapper.element.querySelectorAll('section h2')].map((h2) => h2.textContent)
    expect(headings).toEqual(['连接参数', '一键接入'])
    expect(wrapper.get('[data-testid="key-full"]').text()).toBe(SECRET)
    expect((wrapper.get('[data-testid="model-input"]').element as HTMLInputElement).value).toBe('deepseek-chat')
    expect((wrapper.get('[data-testid="open-codex"]').element as HTMLButtonElement).disabled).toBe(false)
    const table = wrapper.element.querySelector('table')!
    const panel = wrapper.element.querySelector('#g-params')!
    expect(table.compareDocumentPosition(panel) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()
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

  it('列表中的密钥支持切换显示完整密钥与再次弹窗查看', async () => {
    await mountView('127.0.0.1', [keyInfo({ id: 'k1', masked: 'sk-r…a9f2', secret: SECRET })])
    // 初始显示掩码
    expect(wrapper.get('[data-testid="key-value-k1"]').text()).toBe('sk-r…a9f2')

    // 点击小眼睛显示完整密钥
    await wrapper.get('[data-testid="toggle-plain-k1"]').trigger('click')
    expect(wrapper.get('[data-testid="key-value-k1"]').text()).toBe(SECRET)

    // 再次点击隐藏
    await wrapper.get('[data-testid="toggle-plain-k1"]').trigger('click')
    expect(wrapper.get('[data-testid="key-value-k1"]').text()).toBe('sk-r…a9f2')

    // 点击操作列的查看按钮打开弹窗再次查看
    await wrapper.get('[data-testid="view-k1"]').trigger('click')
    expect(q('[data-testid="secret-value"]')?.textContent).toBe(SECRET)
    q('[data-testid="close-secret"]')!.click()
    await flushPromises()
    expect(q('[data-testid="secret-value"]')).toBeNull()
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
