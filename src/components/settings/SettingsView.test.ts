import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { AppSettings, SystemStatus } from '@/types'

const h = vi.hoisted(() => ({
  api: { getSettings: vi.fn(), saveSettings: vi.fn(), systemStatus: vi.fn(), startGateway: vi.fn(), stopGateway: vi.fn() },
}))
vi.mock('@/api', () => ({ api: h.api }))
vi.mock('vue-router', () => ({ onBeforeRouteLeave: vi.fn() }))

import SettingsView from '@/views/SettingsView.vue'
import SettingsDataSection from './SettingsDataSection.vue'
import { useSystemStore } from '@/stores/system'
import { useSettingsStore } from '@/stores/settings'
import { isLoopbackAddress, isValidListenAddress } from './address'
import { version as packageVersion } from '../../../package.json'

const base: AppSettings = {
  listenAddress: '127.0.0.1', port: 20128, autoStart: false, trayEnabled: true, timeoutSeconds: 120, retryCount: 2,
  language: 'zh-CN', theme: 'light', fontScale: 1, redactLogs: true,
}
function status(over: Partial<SystemStatus['gateway']> = {}): SystemStatus {
  return {
    gateway: { running: false, address: '127.0.0.1', port: 20128, error: null, ...over },
    apiRequests: 0, successRate: null, avgLatencyMs: null,
  }
}

let wrapper: VueWrapper
const dialogButton = (text: string) =>
  Array.from(document.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')).find((b) => b.textContent?.trim() === text)

async function mountView(gw: SystemStatus = status()) {
  h.api.getSettings.mockResolvedValue({ ...base })
  h.api.systemStatus.mockResolvedValue(gw)
  h.api.saveSettings.mockImplementation(async (s: AppSettings) => s)
  wrapper = mount(SettingsView, { attachTo: document.body })
  await useSystemStore().refresh()
  await flushPromises()
}
async function setInput(sel: string, value: string) {
  await wrapper.get(sel).setValue(value)
}

beforeEach(() => {
  setActivePinia(createPinia())
  Object.values(h.api).forEach((f) => f.mockReset())
})
afterEach(() => {
  wrapper?.unmount()
  document.body.innerHTML = ''
})

describe('address helpers', () => {
  it('识别回环地址', () => {
    for (const a of ['127.0.0.1', 'localhost', '::1', '127.0.1.1']) expect(isLoopbackAddress(a)).toBe(true)
    for (const a of ['0.0.0.0', '192.168.0.2', '', '::']) expect(isLoopbackAddress(a)).toBe(false)
  })
  it('校验监听地址格式', () => {
    expect(isValidListenAddress('0.0.0.0')).toBe(true)
    expect(isValidListenAddress('999.1.1.1')).toBe(false)
    expect(isValidListenAddress('not an ip')).toBe(false)
  })
})

describe('SettingsView', () => {
  it('渲染五个区块与版本号', async () => {
    await mountView()
    for (const t of ['本地服务', '协议与路由', '数据与安全', '界面', '关于']) expect(wrapper.text()).toContain(t)
    expect(wrapper.get('[data-testid="app-version"]').text()).toBe(packageVersion)
  })

  it('网关状态来自 system store：停止 / 运行 / 端口占用错误', async () => {
    await mountView(status({ error: '端口 20128 已被占用' }))
    expect(wrapper.get('[data-testid="gateway-state"]').text()).toBe('已停止')
    expect(wrapper.get('[data-testid="gateway-error"]').text()).toContain('端口 20128 已被占用')
    expect(wrapper.get('[data-testid="gateway-toggle"]').text()).toContain('启动服务')

    useSystemStore().status = status({ running: true })
    await flushPromises()
    expect(wrapper.get('[data-testid="gateway-state"]').text()).toBe('运行中')
    expect(wrapper.get('[data-testid="gateway-toggle"]').text()).toContain('停止服务')
    expect(wrapper.find('[data-testid="gateway-error"]').exists()).toBe(false)
  })

  it('点击启动后以后端刷新的真实状态为准（启动失败仍显示已停止与错误）', async () => {
    await mountView()
    h.api.startGateway.mockResolvedValue(status({ error: 'x' }).gateway)
    h.api.systemStatus.mockResolvedValue(status({ running: false, error: '绑定失败：地址已被使用' }))
    await wrapper.get('[data-testid="gateway-toggle"]').trigger('click')
    await flushPromises()
    expect(h.api.startGateway).toHaveBeenCalledTimes(1)
    expect(wrapper.get('[data-testid="gateway-state"]').text()).toBe('已停止')
    expect(wrapper.get('[data-testid="gateway-error"]').text()).toContain('绑定失败')
  })

  it('状态未知时不能启停', async () => {
    h.api.getSettings.mockResolvedValue({ ...base })
    h.api.systemStatus.mockRejectedValue(new Error('offline'))
    wrapper = mount(SettingsView, { attachTo: document.body })
    await useSystemStore().refresh()
    await flushPromises()
    expect(wrapper.get('[data-testid="gateway-state"]').text()).toBe('状态未知')
    expect(wrapper.get('[data-testid="gateway-toggle"]').attributes('disabled')).toBeDefined()
  })

  it('改为非回环地址：保存前弹确认并提示需要密钥认证，确认后才保存', async () => {
    await mountView()
    await setInput('#s-addr', '0.0.0.0')
    expect(wrapper.find('[data-testid="expose-warning"]').exists()).toBe(true)
    expect(wrapper.get('[data-testid="dirty-note"]').text()).toContain('未保存')

    await wrapper.get('[data-testid="save"]').trigger('click')
    await flushPromises()
    expect(h.api.saveSettings).not.toHaveBeenCalled()
    expect(document.body.textContent).toContain('密钥认证')

    dialogButton('确认并保存')!.click()
    await flushPromises()
    expect(h.api.saveSettings).toHaveBeenCalledTimes(1)
    expect(h.api.saveSettings.mock.calls[0][0].listenAddress).toBe('0.0.0.0')
    expect(useSettingsStore().dirty).toBe(false)
  })

  it('取消非回环确认则不保存', async () => {
    await mountView()
    await setInput('#s-addr', '0.0.0.0')
    await wrapper.get('[data-testid="save"]').trigger('click')
    dialogButton('取消')!.click()
    await flushPromises()
    expect(h.api.saveSettings).not.toHaveBeenCalled()
    expect(useSettingsStore().dirty).toBe(true)
  })

  it('回环地址下的普通修改直接保存；重置恢复已保存值', async () => {
    await mountView()
    await setInput('#s-timeout', '30')
    await wrapper.get('[data-testid="reset"]').trigger('click')
    expect((wrapper.get('#s-timeout').element as HTMLInputElement).value).toBe('120')
    expect(wrapper.find('[data-testid="dirty-note"]').exists()).toBe(false)

    await setInput('#s-timeout', '30')
    await wrapper.get('[data-testid="save"]').trigger('click')
    await flushPromises()
    expect(h.api.saveSettings).toHaveBeenCalledTimes(1)
    expect(h.api.saveSettings.mock.calls[0][0].timeoutSeconds).toBe(30)
  })

  it('非法端口 / 超时：显示错误并禁止保存', async () => {
    await mountView()
    await setInput('#s-port', '70000')
    await setInput('#s-timeout', '0')
    expect(wrapper.text()).toContain('端口必须是 1–65535')
    expect(wrapper.text()).toContain('超时必须是 1–600')
    expect(wrapper.get('[data-testid="save"]').attributes('disabled')).toBeDefined()
  })

  it('保存失败显示错误并保持未保存状态', async () => {
    await mountView()
    h.api.saveSettings.mockRejectedValue(new Error('写入失败'))
    await setInput('#s-retry', '5')
    await wrapper.get('[data-testid="save"]').trigger('click')
    await flushPromises()
    expect(wrapper.get('[data-testid="save-bar"]').text()).toContain('保存失败：写入失败')
    expect(useSettingsStore().dirty).toBe(true)
  })

  it('协议开关展示已实现状态；备份 / 恢复 / 清空默认禁用并提示待实现', async () => {
    await mountView()
    expect(wrapper.get('[data-testid="protocol-gemini"]').text()).toContain('已实现')
    for (const id of ['btn-backup', 'btn-restore', 'btn-clear']) {
      expect(wrapper.get(`[data-testid="${id}"]`).attributes('disabled')).toBeDefined()
    }
    expect(wrapper.get('[data-testid="btn-clear"]').attributes('title')).toContain('待实现')
  })
})

describe('SettingsDataSection', () => {
  it('清空记录能力可用时，必须经 ConfirmDialog 确认后才触发', async () => {
    setActivePinia(createPinia())
    h.api.getSettings.mockResolvedValue({ ...base })
    await useSettingsStore().load()
    const w = mount(SettingsDataSection, { props: { capabilities: { backup: true, restore: true, clearRecords: true } }, attachTo: document.body })
    await w.get('[data-testid="btn-clear"]').trigger('click')
    expect(w.emitted('clear-records')).toBeUndefined()
    expect(document.body.textContent).toContain('此操作无法撤销')

    dialogButton('取消')!.click()
    await flushPromises()
    expect(w.emitted('clear-records')).toBeUndefined()

    await w.get('[data-testid="btn-clear"]').trigger('click')
    dialogButton('确认清空')!.click()
    await flushPromises()
    expect(w.emitted('clear-records')).toHaveLength(1)
    w.unmount()
  })
})
