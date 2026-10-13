import { defineStore } from 'pinia'
import { computed, reactive, ref } from 'vue'
import { api } from '@/api'
import type { MediaJob, MediaKind, ProviderSummary } from '@/types'
import { errorMessage } from '@/components/media/format'
import { isActiveJob } from '@/components/media/jobStatus'

export interface KindState {
  jobs: MediaJob[]
  loading: boolean
  loaded: boolean
  loadError: string | null
  submitting: boolean
  submitError: string | null
  currentJobId: string | null
  cancelling: string[]
  cancelError: string | null
}

const KINDS: MediaKind[] = ['image', 'asr', 'tts', 'video', 'search']
const POLL_MS = 3000

function blank(): KindState {
  return {
    jobs: [],
    loading: false,
    loaded: false,
    loadError: null,
    submitting: false,
    submitError: null,
    currentJobId: null,
    cancelling: [],
    cancelError: null,
  }
}

export const useMediaStore = defineStore('media', () => {
  const kinds = reactive(Object.fromEntries(KINDS.map((k) => [k, blank()])) as Record<MediaKind, KindState>)

  const providers = ref<ProviderSummary[]>([])
  const providersLoading = ref(false)
  const providersLoaded = ref(false)
  const providersError = ref<string | null>(null)
  /** 仅适配器已真实接入的供应商才可用于提交；其余一律不当作可用 */
  const availableProviders = computed(() => providers.value.filter((p) => p.adapterReady))

  const timers: Partial<Record<MediaKind, ReturnType<typeof setInterval>>> = {}

  async function loadProviders() {
    if (providersLoading.value) return
    providersLoading.value = true
    try {
      providers.value = await api.listProviders()
      providersError.value = null
      providersLoaded.value = true
    } catch (e) {
      providersError.value = errorMessage(e)
    } finally {
      providersLoading.value = false
    }
  }

  function upsertJob(kind: MediaKind, job: MediaJob) {
    const list = kinds[kind].jobs
    const i = list.findIndex((j) => j.id === job.id)
    if (i >= 0) list[i] = job
    else list.unshift(job)
  }

  function stopPolling(kind: MediaKind) {
    const t = timers[kind]
    if (t) clearInterval(t)
    delete timers[kind]
  }

  /** 有排队 / 处理中任务时轮询刷新，全部结束后自动停止。 */
  function syncPolling(kind: MediaKind) {
    const active = kinds[kind].jobs.some((j) => isActiveJob(j.status))
    if (active && !timers[kind]) {
      timers[kind] = setInterval(() => void loadJobs(kind, true), POLL_MS)
    } else if (!active) {
      stopPolling(kind)
    }
  }

  async function loadJobs(kind: MediaKind, silent = false) {
    const st = kinds[kind]
    if (!silent) st.loading = true
    try {
      st.jobs = await api.listMediaJobs(kind)
      st.loadError = null
      st.loaded = true
    } catch (e) {
      st.loadError = errorMessage(e)
    } finally {
      st.loading = false
      syncPolling(kind)
    }
  }

  /** 提交任务。提交中重复调用直接忽略；失败时把错误写入 submitError，不伪造任何结果。 */
  async function submit(kind: MediaKind, params: Record<string, unknown>): Promise<MediaJob | null> {
    const st = kinds[kind]
    if (st.submitting) return null
    st.submitting = true
    st.submitError = null
    try {
      const job = await api.submitMediaJob(kind, params)
      st.currentJobId = job.id
      upsertJob(kind, job)
      syncPolling(kind)
      return job
    } catch (e) {
      st.submitError = errorMessage(e)
      return null
    } finally {
      st.submitting = false
    }
  }

  async function cancel(kind: MediaKind, id: string) {
    const st = kinds[kind]
    if (st.cancelling.includes(id)) return
    st.cancelling.push(id)
    st.cancelError = null
    try {
      upsertJob(kind, await api.cancelMediaJob(id))
      syncPolling(kind)
    } catch (e) {
      st.cancelError = `取消失败：${errorMessage(e)}`
    } finally {
      st.cancelling = st.cancelling.filter((x) => x !== id)
    }
  }

  function currentJob(kind: MediaKind): MediaJob | null {
    const st = kinds[kind]
    return st.jobs.find((j) => j.id === st.currentJobId) ?? null
  }

  return {
    kinds,
    providers,
    providersLoading,
    providersLoaded,
    providersError,
    availableProviders,
    loadProviders,
    loadJobs,
    submit,
    cancel,
    currentJob,
    stopPolling,
  }
})
