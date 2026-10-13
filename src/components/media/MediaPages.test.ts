import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { MediaJob, ProviderSummary } from '@/types'

const h = vi.hoisted(() => ({
  api: { listProviders: vi.fn(), listMediaJobs: vi.fn(), submitMediaJob: vi.fn(), cancelMediaJob: vi.fn() },
}))
vi.mock('@/api', () => ({ api: h.api }))

import ImageView from '@/views/ImageView.vue'
import AsrView from '@/views/AsrView.vue'
import TtsView from '@/views/TtsView.vue'
import VideoView from '@/views/VideoView.vue'
import WebSearchView from '@/views/WebSearchView.vue'

function provider(over: Partial<ProviderSummary> = {}): ProviderSummary {
  return { id: 'p1', name: 'Provider One', group: 'free', status: 'connected', connectionCount: 1, adapterReady: true, protocols: [], quotaHint: null, serviceKinds: ['llm'], website: null, color: null, textIcon: null, authModes: ['apiKey'], pendingReason: null, ...over }
}
function job(over: Partial<MediaJob> = {}): MediaJob {
  return {
    id: 'j1', kind: 'image', providerId: 'p1', model: 'm', status: 'queued', summary: 'summary', outputPath: null, error: null,
    createdAt: '2026-10-14T00:00:00Z', finishedAt: null, ...over,
  }
}

let wrapper: VueWrapper
async function mountPage(comp: object, providers: ProviderSummary[] = [provider()], jobs: MediaJob[] = []) {
  h.api.listProviders.mockResolvedValue(providers)
  h.api.listMediaJobs.mockResolvedValue(jobs)
  wrapper = mount(comp, { attachTo: document.body })
  await flushPromises()
}
const submitForm = async () => {
  await wrapper.get('form').trigger('submit')
  await flushPromises()
}

beforeEach(() => {
  setActivePinia(createPinia())
  Object.values(h.api).forEach((f) => f.mockReset())
  vi.useRealTimers()
})
afterEach(() => {
  wrapper?.unmount()
  document.body.innerHTML = ''
  vi.unstubAllGlobals()
})

describe.each([
  ['图像', ImageView],
  ['语音转文字', AsrView],
  ['文字转语音', TtsView],
  ['视频', VideoView],
  ['WebSearch', WebSearchView],
])('%s 页：无可用供应商', (_name, comp) => {
  it('没有适配器就绪的供应商时显示 EmptyState 并说明原因，不渲染提交表单', async () => {
    await mountPage(comp, [provider({ adapterReady: false })])
    expect(wrapper.text()).toContain('暂无可用供应商')
    expect(wrapper.text()).toContain('适配器尚未接入')
    expect(wrapper.find('form').exists()).toBe(false)
  })
})

describe('图像页', () => {
  it('必填提示词为空不提交', async () => {
    await mountPage(ImageView)
    await submitForm()
    expect(h.api.submitMediaJob).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('请填写提示词')
  })

  it('提交失败显示错误，且不出现任何生成结果', async () => {
    await mountPage(ImageView)
    h.api.submitMediaJob.mockRejectedValue(new Error('后端媒体适配器尚未接入，无法提交任务'))
    await wrapper.get('#' + wrapper.get('[data-field="prompt"] textarea').attributes('id')).setValue('a cat')
    await submitForm()
    expect(h.api.submitMediaJob).toHaveBeenCalledTimes(1)
    const [kind, params] = h.api.submitMediaJob.mock.calls[0]
    expect(kind).toBe('image')
    expect(params).toMatchObject({ prompt: 'a cat', providerId: 'p1', mode: 'generate', size: '1024x1024', n: 1 })
    expect(wrapper.get('[data-testid="submit-error"]').text()).toContain('尚未接入')
    expect(wrapper.find('[data-testid="no-job"]').exists()).toBe(true)
    expect(wrapper.find('img').exists()).toBe(false)
  })

  it('提交中按钮禁用，重复提交被忽略', async () => {
    await mountPage(ImageView)
    let resolve!: (j: MediaJob) => void
    h.api.submitMediaJob.mockReturnValue(new Promise<MediaJob>((r) => (resolve = r)))
    await wrapper.get('[data-field="prompt"] textarea').setValue('a cat')
    await wrapper.get('form').trigger('submit')
    await flushPromises()
    expect(wrapper.get('[data-testid="submit"]').attributes('disabled')).toBeDefined()
    await wrapper.get('form').trigger('submit')
    await flushPromises()
    expect(h.api.submitMediaJob).toHaveBeenCalledTimes(1)
    resolve(job({ status: 'running' }))
    await flushPromises()
    expect(wrapper.get('[data-testid="submit"]').attributes('disabled')).toBeUndefined()
    expect(wrapper.get('[data-testid="job-status"]').text()).toBe('处理中')
  })

  it('图片编辑 Tab 才出现图片字段，切换时保留已填提示词', async () => {
    await mountPage(ImageView)
    expect(wrapper.find('[data-field="image"]').exists()).toBe(false)
    await wrapper.get('[data-field="prompt"] textarea').setValue('keep me')
    await wrapper.get('[data-testid="tab-edit"]').trigger('click')
    expect(wrapper.find('[data-field="image"]').exists()).toBe(true)
    expect((wrapper.get('[data-field="prompt"] textarea').element as HTMLTextAreaElement).value).toBe('keep me')
  })
})

describe('视频页', () => {
  it('排队中的任务可取消，取消后状态更新', async () => {
    await mountPage(VideoView, [provider()], [])
    h.api.submitMediaJob.mockResolvedValue(job({ kind: 'video', status: 'queued' }))
    h.api.cancelMediaJob.mockResolvedValue(job({ kind: 'video', status: 'cancelled' }))
    await wrapper.get('[data-field="prompt"] textarea').setValue('a drone shot')
    await submitForm()
    expect(wrapper.get('[data-testid="job-status"]').text()).toBe('排队中')
    await wrapper.get('[data-testid="cancel-job"]').trigger('click')
    await flushPromises()
    expect(h.api.cancelMediaJob).toHaveBeenCalledWith('j1')
    expect(wrapper.get('[data-testid="job-status"]').text()).toBe('已取消')
    expect(wrapper.find('[data-testid="cancel-job"]').exists()).toBe(false)
  })

  it('失败任务显示错误原因', async () => {
    await mountPage(VideoView, [provider()], [job({ kind: 'video', status: 'failed', error: '上游额度不足' })])
    h.api.submitMediaJob.mockResolvedValue(job({ kind: 'video', status: 'failed', error: '上游额度不足' }))
    await wrapper.get('[data-field="prompt"] textarea').setValue('x')
    await submitForm()
    expect(wrapper.get('[data-testid="job-error"]').text()).toContain('上游额度不足')
  })
})

describe('语音转文字页', () => {
  it('时间戳 / 说话人分离在无特性声明时禁用', async () => {
    await mountPage(AsrView)
    const sw = wrapper.findAll('[role="switch"]')
    expect(sw).toHaveLength(2)
    sw.forEach((s) => expect(s.attributes('disabled')).toBeDefined())
  })

  it('不支持的音频格式被拒绝并提示', async () => {
    await mountPage(AsrView)
    const input = wrapper.get('[data-testid="file-input"]').element as HTMLInputElement
    const f = new File(['x'], 'evil.exe')
    Object.defineProperty(input, 'files', { value: [f], configurable: true })
    await wrapper.get('[data-testid="file-input"]').trigger('change')
    expect(wrapper.text()).toContain('不支持的文件格式')
  })

  it('未确认隐私提示不能提交；确认并选择合法文件后提交', async () => {
    await mountPage(AsrView)
    const input = wrapper.get('[data-testid="file-input"]').element as HTMLInputElement
    Object.defineProperty(input, 'files', { value: [new File(['abc'], 'a.mp3', { type: 'audio/mpeg' })], configurable: true })
    await wrapper.get('[data-testid="file-input"]').trigger('change')
    await submitForm()
    expect(h.api.submitMediaJob).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('请先确认已了解音频将上传')

    h.api.submitMediaJob.mockResolvedValue(job({ kind: 'asr', status: 'succeeded', summary: '你好世界' }))
    await wrapper.get('[data-testid="ack"]').setValue(true)
    await submitForm()
    // 文件经 FileReader 异步读取，需等待提交真正发生
    await vi.waitFor(() => expect(h.api.submitMediaJob).toHaveBeenCalledTimes(1))
    await flushPromises()
    expect(h.api.submitMediaJob.mock.calls[0][1].audio).toMatchObject({ name: 'a.mp3', mimeType: 'audio/mpeg' })
    expect(wrapper.get('[data-testid="transcript"]').text()).toBe('你好世界')
  })

  it('麦克风权限被拒绝时给出明确提示', async () => {
    await mountPage(AsrView)
    vi.stubGlobal('MediaRecorder', class {})
    Object.defineProperty(navigator, 'mediaDevices', {
      configurable: true,
      value: { getUserMedia: vi.fn().mockRejectedValue(Object.assign(new Error('denied'), { name: 'NotAllowedError' })) },
    })
    await wrapper.get('[data-testid="record"]').trigger('click')
    await flushPromises()
    expect(wrapper.get('[data-testid="record-error"]').text()).toContain('麦克风权限被拒绝')
  })

  it('环境不支持录音时提示改用上传', async () => {
    await mountPage(AsrView)
    vi.stubGlobal('MediaRecorder', undefined)
    await wrapper.get('[data-testid="record"]').trigger('click')
    await flushPromises()
    expect(wrapper.get('[data-testid="record-error"]').text()).toContain('不支持麦克风录音')
  })
})

describe('文字转语音页', () => {
  it('显示最大字数；云端与本地分开标记，本地为待实现', async () => {
    await mountPage(TtsView)
    expect(wrapper.get('[data-testid="tts-limits"]').text()).toContain('4096')
    expect(wrapper.text()).toContain('云端 TTS')
    expect(wrapper.text()).toContain('本地 TTS · 待实现')
  })

  it('超长文本不提交；音调在无声明时禁用；试听带 intent=preview', async () => {
    await mountPage(TtsView)
    expect(wrapper.get('[data-field="pitch"] input').attributes('disabled')).toBeDefined()
    await wrapper.get('[data-field="voice"] input').setValue('alloy')
    await wrapper.get('[data-field="text"] textarea').setValue('a'.repeat(4097))
    await wrapper.get('[data-testid="preview"]').trigger('click')
    await flushPromises()
    expect(h.api.submitMediaJob).not.toHaveBeenCalled()

    h.api.submitMediaJob.mockResolvedValue(job({ kind: 'tts', status: 'running' }))
    await wrapper.get('[data-field="text"] textarea').setValue('你好')
    await wrapper.get('[data-testid="preview"]').trigger('click')
    await flushPromises()
    expect(h.api.submitMediaJob.mock.calls[0][1]).toMatchObject({ text: '你好', voice: 'alloy', engine: 'cloud', intent: 'preview' })
  })
})

describe('WebSearch 页', () => {
  it('区分原生接口与聚合结果，链接仅允许 http(s)', async () => {
    const payload = (origin: string) =>
      JSON.stringify({ origin, results: [{ title: 'Doc', url: 'https://example.com/doc', snippet: 'sn' }, { title: 'bad', url: 'javascript:alert(1)' }] })
    await mountPage(WebSearchView)
    h.api.submitMediaJob.mockResolvedValue(job({ kind: 'search', status: 'succeeded', summary: payload('aggregated') }))
    await wrapper.get('[data-field="query"] input').setValue('vue')
    await submitForm()
    expect(h.api.submitMediaJob.mock.calls[0][1]).toMatchObject({ query: 'vue', resultMode: 'native', count: 5 })
    expect(wrapper.get('[data-testid="origin-tag"]').text()).toContain('聚合结果')
    const links = wrapper.findAll('ol a')
    expect(links).toHaveLength(1)
    expect(links[0].attributes('href')).toBe('https://example.com/doc')
    expect(links[0].attributes('rel')).toContain('noopener')
  })

  it('无法解析的返回只显示原始文本，不构造结果', async () => {
    await mountPage(WebSearchView)
    h.api.submitMediaJob.mockResolvedValue(job({ kind: 'search', status: 'succeeded', summary: 'plain text' }))
    await wrapper.get('[data-field="query"] input').setValue('vue')
    await submitForm()
    expect(wrapper.find('ol').exists()).toBe(false)
    expect(wrapper.text()).toContain('无法解析结构化搜索结果')
  })
})
