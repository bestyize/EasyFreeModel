import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { mount, flushPromises } from '@vue/test-utils'
import { APP_VERSION } from '@/router/nav'

const h = vi.hoisted(() => ({ checkForUpdate: vi.fn(), openExternal: vi.fn() }))
vi.mock('@/api', () => ({ api: { checkForUpdate: h.checkForUpdate } }))
vi.mock('@/api/openExternal', () => ({ openExternal: h.openExternal }))

import { isNewerVersion, UPDATE_INTERVAL_MS, useUpdateStore } from './update'
import UpdateNotice from '@/components/UpdateNotice.vue'
import SettingsAboutSection from '@/components/settings/SettingsAboutSection.vue'

beforeEach(() => {
  setActivePinia(createPinia())
  vi.useFakeTimers()
  vi.setSystemTime(new Date('2026-10-16T00:00:00Z'))
  h.checkForUpdate.mockReset().mockResolvedValue(null)
  h.openExternal.mockReset().mockResolvedValue(undefined)
})
afterEach(() => {
  useUpdateStore().stop()
  vi.useRealTimers()
})

describe('update checks', () => {
  it('compares numeric version components, rejecting malformed versions', () => {
    expect(isNewerVersion('0.8.10', '0.8.9')).toBe(true)
    expect(isNewerVersion('0.9.0', '0.8.99')).toBe(true)
    expect(isNewerVersion('1.0.0', '0.99.99')).toBe(true)
    for (const remote of ['0.8.9', '0.8.11', '0.7.99', '', '404', '0.9', '0.9.0-beta', '0.9.0\ntext']) {
      expect(isNewerVersion(remote, '0.8.11'), remote).toBe(false)
    }
  })

  it('checks once at startup and every 24 hours, without duplicate timers', async () => {
    const store = useUpdateStore()
    store.start()
    store.start()
    await flushPromises()
    expect(h.checkForUpdate).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(UPDATE_INTERVAL_MS - 1)
    expect(h.checkForUpdate).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1)
    expect(h.checkForUpdate).toHaveBeenCalledTimes(2)
    store.stop()
    await vi.advanceTimersByTimeAsync(UPDATE_INTERVAL_MS)
    expect(h.checkForUpdate).toHaveBeenCalledTimes(2)
  })

  it('catches up on wake without repeated focus/visibility requests', async () => {
    const store = useUpdateStore()
    store.start()
    await flushPromises()
    window.dispatchEvent(new Event('focus'))
    expect(h.checkForUpdate).toHaveBeenCalledTimes(1)
    vi.setSystemTime(Date.now() + UPDATE_INTERVAL_MS)
    window.dispatchEvent(new Event('focus'))
    document.dispatchEvent(new Event('visibilitychange'))
    await flushPromises()
    expect(h.checkForUpdate).toHaveBeenCalledTimes(2)
    store.stop()
    vi.setSystemTime(Date.now() + UPDATE_INTERVAL_MS)
    window.dispatchEvent(new Event('focus'))
    expect(h.checkForUpdate).toHaveBeenCalledTimes(2)
  })

  it('schedules the next automatic check 24 hours after a manual check', async () => {
    const store = useUpdateStore()
    store.start()
    await flushPromises()
    await vi.advanceTimersByTimeAsync(23 * 60 * 60 * 1000)
    await store.check()
    expect(h.checkForUpdate).toHaveBeenCalledTimes(2)
    await vi.advanceTimersByTimeAsync(UPDATE_INTERVAL_MS - 1)
    expect(h.checkForUpdate).toHaveBeenCalledTimes(2)
    await vi.advanceTimersByTimeAsync(1)
    expect(h.checkForUpdate).toHaveBeenCalledTimes(3)
  })

  it('does not show a notice on failures, equal, older or invalid versions', async () => {
    const store = useUpdateStore()
    for (const result of [null, APP_VERSION, '0.0.1', 'bad-version']) {
      h.checkForUpdate.mockResolvedValueOnce(result)
      await store.check()
      expect(store.noticeVisible).toBe(false)
      expect(store.latestVersion).toBeNull()
    }
    h.checkForUpdate.mockRejectedValueOnce(new Error('offline'))
    await expect(store.check()).resolves.toBeUndefined()
    expect(store.noticeVisible).toBe(false)
    expect(store.checking).toBe(false)
  })

  it('does not launch overlapping requests', async () => {
    let finish!: (version: string | null) => void
    h.checkForUpdate.mockReturnValueOnce(new Promise((resolve) => { finish = resolve }))
    const store = useUpdateStore()
    const pending = store.check()
    await store.check()
    expect(h.checkForUpdate).toHaveBeenCalledTimes(1)
    expect(store.checking).toBe(true)
    finish(null)
    await pending
    expect(store.checking).toBe(false)
  })

  it('shows a dismissible notice and opens the exact published release', async () => {
    const store = useUpdateStore()
    const wrapper = mount(UpdateNotice)
    expect(wrapper.find('[role="status"]').exists()).toBe(false)
    h.checkForUpdate.mockResolvedValue('99.0.0')
    await store.check()
    await wrapper.vm.$nextTick()
    expect(wrapper.text()).toContain('FreeApiBox v99.0.0 已发布')
    await wrapper.get('.btn').trigger('click')
    expect(h.openExternal).toHaveBeenCalledWith('https://github.com/bestyize/FreeApiBox/releases/tag/v99.0.0')
    await wrapper.get('[aria-label="暂不更新"]').trigger('click')
    expect(wrapper.find('[role="status"]').exists()).toBe(false)
    await store.check()
    expect(store.noticeVisible).toBe(false)
    h.checkForUpdate.mockResolvedValueOnce('99.0.1')
    await store.check()
    expect(store.noticeVisible).toBe(true)
    wrapper.unmount()
  })

  it('keeps a manual check and download entry in Settings after dismissing the notice', async () => {
    const store = useUpdateStore()
    const wrapper = mount(SettingsAboutSection)
    h.checkForUpdate.mockResolvedValue('99.0.0')
    await wrapper.findAll('button').find((b) => b.text() === '检查更新')!.trigger('click')
    await flushPromises()
    store.dismiss()
    await wrapper.vm.$nextTick()
    expect(wrapper.text()).toContain('下载 v99.0.0')
    await wrapper.findAll('button').find((b) => b.text() === '下载 v99.0.0')!.trigger('click')
    expect(h.openExternal).toHaveBeenCalledWith('https://github.com/bestyize/FreeApiBox/releases/tag/v99.0.0')
    wrapper.unmount()
  })
})
