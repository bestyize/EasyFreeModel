import { defineStore } from 'pinia'
import { ref } from 'vue'
import { api } from '@/api'
import type { SystemStatus } from '@/types'

export const useSystemStore = defineStore('system', () => {
  const status = ref<SystemStatus | null>(null)
  const loadError = ref<string | null>(null)
  let timer: ReturnType<typeof setInterval> | undefined

  async function refresh() {
    try {
      status.value = await api.systemStatus()
      loadError.value = null
    } catch (e) {
      loadError.value = e instanceof Error ? e.message : String(e)
    }
  }

  function startPolling(intervalMs = 5000) {
    stopPolling()
    void refresh()
    timer = setInterval(refresh, intervalMs)
  }

  function stopPolling() {
    if (timer) clearInterval(timer)
    timer = undefined
  }

  async function toggleGateway() {
    const running = status.value?.gateway.running
    try {
      await (running ? api.stopGateway() : api.startGateway())
    } finally {
      await refresh()
    }
  }

  return { status, loadError, refresh, startPolling, stopPolling, toggleGateway }
})
