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
  const themeError = ref<string | null>(null)
  let pendingLoad: Promise<void> | null = null
  let systemThemeQuery: MediaQueryList | null = null

  const dirty = computed(() => !!saved.value && !!draft.value && !sameSettings(saved.value, draft.value))

  function load(force = false): Promise<void> {
    if (pendingLoad) return pendingLoad
    // 草稿有未保存修改时不覆盖，除非强制
    if (dirty.value && !force) return Promise.resolve()
    loading.value = true
    pendingLoad = (async () => {
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
        pendingLoad = null
      }
    })()
    return pendingLoad
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
      themeError.value = null
      applyAppearance(result)
      return true
    } catch (e) {
      saveError.value = errorMessage(e)
      return false
    } finally {
      saving.value = false
    }
  }

  /** 顶部快捷按钮仅保存主题，不会顺带提交设置页其他未保存的字段。 */
  async function toggleTheme(): Promise<boolean> {
    if (saving.value) return false
    await ensureLoaded()
    if (!saved.value || saving.value) {
      themeError.value = loadError.value ?? '加载主题设置失败'
      return false
    }
    const previous = clone(saved.value)
    const nextTheme = useUiStore().theme === 'dark' ? 'light' : 'dark'
    const next: AppSettings = { ...previous, theme: nextTheme }
    themeError.value = null
    saving.value = true
    applyAppearance(next)
    try {
      const result = await api.saveSettings(next)
      saved.value = clone(result)
      if (draft.value && draft.value.theme === previous.theme) draft.value.theme = result.theme
      applyAppearance(result)
      return true
    } catch (e) {
      themeError.value = `主题保存失败：${errorMessage(e)}`
      applyAppearance(previous)
      return false
    } finally {
      saving.value = false
    }
  }

  function startAppearanceSync() {
    if (systemThemeQuery || typeof window === 'undefined' || typeof window.matchMedia !== 'function') return
    systemThemeQuery = window.matchMedia('(prefers-color-scheme: dark)')
    systemThemeQuery.addEventListener('change', onSystemThemeChange)
  }

  function onSystemThemeChange() {
    if (!saving.value && saved.value?.theme === 'system') applyAppearance(saved.value)
  }

  function stopAppearanceSync() {
    systemThemeQuery?.removeEventListener('change', onSystemThemeChange)
    systemThemeQuery = null
  }

  function reset() {
    if (saved.value) draft.value = clone(saved.value)
    saveError.value = null
  }

  return { saved, draft, loading, saving, loadError, saveError, themeError, dirty, load, ensureLoaded, save, toggleTheme, startAppearanceSync, stopAppearanceSync, reset }
})
