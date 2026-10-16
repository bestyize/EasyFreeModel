import { computed, ref } from 'vue'
import { defineStore } from 'pinia'
import { api } from '@/api'
import { openExternal } from '@/api/openExternal'
import { APP_VERSION } from '@/router/nav'

export const UPDATE_INTERVAL_MS = 24 * 60 * 60 * 1000

// Compare components rather than strings or decimal numbers (0.8.10 > 0.8.9).
export function isNewerVersion(remote: string, current: string): boolean {
  const parse = (v: string) => /^\d+\.\d+\.\d+$/.test(v) ? v.split('.').map(BigInt) : null
  const a = parse(remote)
  const b = parse(current)
  if (!a || !b) return false
  for (let i = 0; i < 3; i++) {
    if (a[i] !== b[i]) return a[i] > b[i]
  }
  return false
}

export const useUpdateStore = defineStore('update', () => {
  const latestVersion = ref<string | null>(null)
  const checking = ref(false)
  const dismissedVersion = ref<string | null>(null)
  const noticeVisible = computed(() => !!latestVersion.value && latestVersion.value !== dismissedVersion.value)
  let lastAttempt: number | null = null
  let timer: ReturnType<typeof setTimeout> | undefined
  let active = false

  async function check() {
    if (checking.value) return
    checking.value = true
    clearTimeout(timer)
    lastAttempt = Date.now()
    try {
      const remote = await api.checkForUpdate()
      latestVersion.value = remote && isNewerVersion(remote, APP_VERSION) ? remote : null
    } catch {
      // Checking is optional: offline/IPC failures must not interrupt normal use.
      latestVersion.value = null
    } finally {
      checking.value = false
      if (active) timer = setTimeout(checkIfDue, Math.max(0, lastAttempt + UPDATE_INTERVAL_MS - Date.now()))
    }
  }

  function checkIfDue() {
    if (lastAttempt === null || Date.now() - lastAttempt >= UPDATE_INTERVAL_MS) void check()
  }
  function onVisible() {
    if (document.visibilityState === 'visible') checkIfDue()
  }
  function start() {
    if (active) return
    active = true
    void check()
    // Catch up after a sleeping computer or throttled tray webview resumes.
    window.addEventListener('focus', checkIfDue)
    document.addEventListener('visibilitychange', onVisible)
  }
  function stop() {
    active = false
    clearTimeout(timer)
    timer = undefined
    window.removeEventListener('focus', checkIfDue)
    document.removeEventListener('visibilitychange', onVisible)
  }
  function dismiss() {
    dismissedVersion.value = latestVersion.value
  }
  async function download() {
    const version = latestVersion.value
    if (!version) return
    try {
      await openExternal(`https://github.com/bestyize/FreeApiBox/releases/tag/v${version}`)
    } catch {
      // A failed browser launch must not break the app or produce an unhandled rejection.
    }
  }
  return { latestVersion, checking, noticeVisible, check, start, stop, dismiss, download }
})
