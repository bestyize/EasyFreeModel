import { createPinia, setActivePinia } from 'pinia'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { AppSettings } from '@/types'

const h = vi.hoisted(() => ({ api: { getSettings: vi.fn(), saveSettings: vi.fn() } }))
vi.mock('@/api', () => ({ api: h.api }))

import { useSettingsStore } from './settings'
import { useUiStore } from './ui'

const base: AppSettings = {
  listenAddress: '127.0.0.1', port: 20128, autoStart: false, trayEnabled: true, timeoutSeconds: 120, retryCount: 2,
  language: 'zh-CN', theme: 'light', fontScale: 1, redactLogs: true,
}

beforeEach(() => {
  setActivePinia(createPinia())
  Object.values(h.api).forEach((f) => f.mockReset())
  h.api.getSettings.mockResolvedValue({ ...base })
})
afterEach(() => vi.unstubAllGlobals())

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

  it('快捷切换会持久化主题，保留设置页其他未保存的修改', async () => {
    const s = useSettingsStore()
    await s.load()
    s.draft!.port = 8080
    h.api.saveSettings.mockImplementation(async (input: AppSettings) => input)
    expect(await s.toggleTheme()).toBe(true)
    expect(h.api.saveSettings.mock.calls[0][0]).toMatchObject({ theme: 'dark', port: 20128 })
    expect(s.draft).toMatchObject({ theme: 'dark', port: 8080 })
    expect(s.dirty).toBe(true)
    expect(useUiStore().theme).toBe('dark')
    expect(document.documentElement.dataset.theme).toBe('dark')
    expect(await s.toggleTheme()).toBe(true)
    expect(s.saved!.theme).toBe('light')
  })

  it('快捷切换保存失败会恢复原主题并显示错误', async () => {
    const s = useSettingsStore()
    await s.load()
    h.api.saveSettings.mockRejectedValue(new Error('磁盘写入失败'))
    expect(await s.toggleTheme()).toBe(false)
    expect(document.documentElement.dataset.theme).toBe('light')
    expect(useUiStore().theme).toBe('light')
    expect(s.themeError).toContain('磁盘写入失败')
  })

  it('跟随系统时响应系统主题变化，手动选择后停止跟随', async () => {
    let dark = false
    let onChange: (() => void) | undefined
    vi.stubGlobal('matchMedia', vi.fn(() => ({
      get matches() { return dark },
      addEventListener: (_event: string, fn: () => void) => { onChange = fn },
      removeEventListener: vi.fn(),
    })))
    h.api.getSettings.mockResolvedValue({ ...base, theme: 'system' })
    const s = useSettingsStore()
    s.startAppearanceSync()
    await s.load()
    expect(document.documentElement.dataset.theme).toBe('light')
    dark = true
    onChange!()
    expect(document.documentElement.dataset.theme).toBe('dark')
    h.api.saveSettings.mockImplementation(async (input: AppSettings) => input)
    await s.toggleTheme()
    expect(s.saved!.theme).toBe('light')
    dark = true
    onChange!()
    expect(document.documentElement.dataset.theme).toBe('light')
    s.stopAppearanceSync()
  })
})
