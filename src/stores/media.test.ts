import { flushPromises } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { MediaJob } from '@/types'

const h = vi.hoisted(() => ({
  api: { listProviders: vi.fn(), listMediaJobs: vi.fn(), submitMediaJob: vi.fn(), cancelMediaJob: vi.fn() },
}))
vi.mock('@/api', () => ({ api: h.api }))

import { useMediaStore } from './media'

function job(over: Partial<MediaJob> = {}): MediaJob {
  return {
    id: 'j1', kind: 'video', providerId: 'p', model: '', status: 'queued', summary: '', outputPath: null, error: null,
    createdAt: '2026-10-14T00:00:00Z', finishedAt: null, ...over,
  }
}

beforeEach(() => {
  setActivePinia(createPinia())
  Object.values(h.api).forEach((f) => f.mockReset())
})
afterEach(() => vi.useRealTimers())

describe('media store', () => {
  it('只有适配器就绪的供应商才可用', async () => {
    h.api.listProviders.mockResolvedValue([
      { id: 'a', adapterReady: true },
      { id: 'b', adapterReady: false },
    ])
    const s = useMediaStore()
    await s.loadProviders()
    expect(s.availableProviders.map((p) => p.id)).toEqual(['a'])
  })

  it('提交失败写入 submitError，不产生任务', async () => {
    h.api.submitMediaJob.mockRejectedValue('适配器未接入')
    const s = useMediaStore()
    expect(await s.submit('image', {})).toBeNull()
    expect(s.kinds.image.submitError).toBe('适配器未接入')
    expect(s.kinds.image.jobs).toEqual([])
    expect(s.kinds.image.submitting).toBe(false)
  })

  it('提交中重复提交被忽略', async () => {
    let resolve!: (j: MediaJob) => void
    h.api.submitMediaJob.mockReturnValue(new Promise<MediaJob>((r) => (resolve = r)))
    const s = useMediaStore()
    const first = s.submit('video', {})
    expect(await s.submit('video', {})).toBeNull()
    resolve(job({ status: 'succeeded' }))
    await first
    expect(h.api.submitMediaJob).toHaveBeenCalledTimes(1)
  })

  it('有进行中任务时轮询，完成后自动停止', async () => {
    vi.useFakeTimers()
    h.api.listMediaJobs.mockResolvedValueOnce([job({ status: 'running' })]).mockResolvedValueOnce([job({ status: 'succeeded' })])
    const s = useMediaStore()
    await s.loadJobs('video')
    expect(s.kinds.video.jobs[0].status).toBe('running')
    await vi.advanceTimersByTimeAsync(3000)
    expect(s.kinds.video.jobs[0].status).toBe('succeeded')
    await vi.advanceTimersByTimeAsync(10000)
    expect(h.api.listMediaJobs).toHaveBeenCalledTimes(2)
  })

  it('取消失败时保留任务并给出错误', async () => {
    h.api.submitMediaJob.mockResolvedValue(job({ status: 'running' }))
    h.api.cancelMediaJob.mockRejectedValue(new Error('已无法取消'))
    const s = useMediaStore()
    await s.submit('video', {})
    await s.cancel('video', 'j1')
    await flushPromises()
    expect(s.kinds.video.cancelError).toContain('已无法取消')
    expect(s.currentJob('video')?.status).toBe('running')
    s.stopPolling('video')
  })
})
