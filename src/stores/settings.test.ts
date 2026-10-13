import { createPinia, setActivePinia } from 'pinia'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { AppSettings } from '@/types'

const h = vi.hoisted(() => ({ api: { getSettings: vi.fn(), saveSettings: vi.fn() } }))
vi.mock('@/api', () => ({ api: h.api }))

import { useSettingsStore } from './settings'

const base: AppSettings = {
  listenAddress: '127.0.0.1', port: 20128, autoStart: false, trayEnabled: true, timeoutSeconds: 120, retryCount: 2,
  language: 'zh-CN', theme: 'light', fontScale: 1, redactLogs: true,
}

beforeEach(() => {
  setActivePinia(createPinia())
  Object.values(h.api).forEach((f) => f.mockReset())
  h.api.getSettings.mockResolvedValue({ ...base })
})

describe('settings store', () => {
  it('草稿与已保存值分离，dirty / reset 正常', async () => {
    const s = useSettingsStore()
    await s.load()
    expect(s.dirty).toBe(false)
    s.draft!.port = 8080
    expect(s.dirty).toBe(true)
    expect(s.saved!.port).toBe(20128)
    s.reset()
    expect(s.dirty).toBe(false)
  })

  it('有未保存草稿时普通刷新不覆盖', async () => {
    const s = useSettingsStore()
    await s.load()
    s.draft!.port = 8080
    await s.load()
    expect(s.draft!.port).toBe(8080)
    await s.load(true)
    expect(s.draft!.port).toBe(20128)
  })

  it('保存成功采用后端返回值；失败保留草稿并记录错误', async () => {
    const s = useSettingsStore()
    await s.load()
    s.draft!.retryCount = 5
    h.api.saveSettings.mockResolvedValueOnce({ ...base, retryCount: 5 })
    expect(await s.save()).toBe(true)
    expect(s.saved!.retryCount).toBe(5)

    s.draft!.retryCount = 7
    h.api.saveSettings.mockRejectedValueOnce(new Error('disk full'))
    expect(await s.save()).toBe(false)
    expect(s.saveError).toBe('disk full')
    expect(s.dirty).toBe(true)
  })

  it('应用主题与字体缩放到文档根节点', async () => {
    h.api.getSettings.mockResolvedValue({ ...base, theme: 'dark', fontScale: 1.25 })
    await useSettingsStore().load()
    expect(document.documentElement.dataset.theme).toBe('dark')
    expect(document.documentElement.style.getPropertyValue('--font-scale')).toBe('1.25')
  })
})
