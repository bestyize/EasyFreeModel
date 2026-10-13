import { afterEach, describe, expect, it, vi } from 'vitest'

const invoke = vi.hoisted(() => vi.fn())
vi.mock('@tauri-apps/api/core', () => ({ invoke }))

import { openExternal } from './openExternal'

afterEach(() => {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__
  invoke.mockReset()
  vi.restoreAllMocks()
})

describe('openExternal', () => {
  it('拒绝非 http/https 链接', async () => {
    const open = vi.spyOn(window, 'open').mockReturnValue(null)
    for (const bad of ['javascript:alert(1)', 'file:///etc/passwd', 'data:text/html,hi', 'ftp://example.com', 'not a url', '']) {
      await expect(openExternal(bad)).rejects.toThrow()
    }
    expect(open).not.toHaveBeenCalled()
    expect(invoke).not.toHaveBeenCalled()
  })

  it('浏览器预览：window.open 带 noopener,noreferrer', async () => {
    const open = vi.spyOn(window, 'open').mockReturnValue(null)
    await openExternal('https://example.com/a?b=1')
    expect(open).toHaveBeenCalledWith('https://example.com/a?b=1', '_blank', 'noopener,noreferrer')
    expect(invoke).not.toHaveBeenCalled()
  })

  it('Tauri：走 opener 插件命令', async () => {
    ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {}
    const open = vi.spyOn(window, 'open').mockReturnValue(null)
    invoke.mockResolvedValue(undefined)
    await openExternal('http://127.0.0.1:1455/x')
    expect(invoke).toHaveBeenCalledWith('plugin:opener|open_url', { url: 'http://127.0.0.1:1455/x' })
    expect(open).not.toHaveBeenCalled()
  })
})
