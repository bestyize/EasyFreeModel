import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { api } from '@/api'
import type { AppSettings } from '@/types'
import { errorMessage } from '@/components/media/format'
import { useUiStore } from '@/stores/ui'

function clone(s: AppSettings): AppSettings {
  return { ...s }
}

function sameSettings(a: AppSettings, b: AppSettings): boolean {
  return (Object.keys(a) as (keyof AppSettings)[]).every((k) => a[k] === b[k])
}

/** 把主题与字体缩放应用到文档根节点。样式层按 data-theme / --font-scale 消费。 */
function applyAppearance(s: AppSettings) {
  if (typeof document === 'undefined') return
  let theme: 'light' | 'dark'
  if (s.theme === 'system') {
    theme =
      typeof window !== 'undefined' && typeof window.matchMedia === 'function' && window.matchMedia('(prefers-color-scheme: dark)').matches
        ? 'dark'
        : 'light'
  } else {
    theme = s.theme
  }
  document.documentElement.dataset.theme = theme
  document.documentElement.style.setProperty('--font-scale', String(s.fontScale))
  useUiStore().theme = theme
}

export const useSettingsStore = defineStore('settings', () => {
  /** 后端已持久化的设置 */
  const saved = ref<AppSettings | null>(null)
  /** 表单编辑中的草稿 */
  const draft = ref<AppSettings | null>(null)
  const loading = ref(false)
  const saving = ref(false)
  const loadError = ref<string | null>(null)
  const saveError = ref<string | null>(null)

  const dirty = computed(() => !!saved.value && !!draft.value && !sameSettings(saved.value, draft.value))

  async function load(force = false) {
    if (loading.value) return
    // 草稿有未保存修改时不覆盖，除非强制
    if (dirty.value && !force) return
    loading.value = true
    try {
      const s = await api.getSettings()
      saved.value = clone(s)
      draft.value = clone(s)
      loadError.value = null
      applyAppearance(s)
    } catch (e) {
      loadError.value = errorMessage(e)
    } finally {
      loading.value = false
    }
  }

  /** 页面只需要“已加载过一次”，避免重复请求。 */
  async function ensureLoaded() {
    if (!saved.value) await load()
  }

  async function save(): Promise<boolean> {
    if (!draft.value || saving.value) return false
    saving.value = true
    saveError.value = null
    try {
      const result = await api.saveSettings(clone(draft.value))
      saved.value = clone(result)
      draft.value = clone(result)
      applyAppearance(result)
      return true
    } catch (e) {
      saveError.value = errorMessage(e)
      return false
    } finally {
      saving.value = false
    }
  }

  function reset() {
    if (saved.value) draft.value = clone(saved.value)
    saveError.value = null
  }

  return { saved, draft, loading, saving, loadError, saveError, dirty, load, ensureLoaded, save, reset }
})
