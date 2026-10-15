<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { Moon, Search, Settings, Sun } from 'lucide-vue-next'
import { useUiStore } from '@/stores/ui'
import { useSettingsStore } from '@/stores/settings'

const route = useRoute()
const router = useRouter()
const ui = useUiStore()
const settings = useSettingsStore()
const searchEl = ref<HTMLInputElement | null>(null)
const isMac = typeof navigator !== 'undefined' && /Mac/i.test(navigator.platform)

function onKey(e: KeyboardEvent) {
  if ((isMac ? e.metaKey : e.ctrlKey) && e.key.toLowerCase() === 'k') {
    e.preventDefault()
    searchEl.value?.focus()
  }
}
onMounted(() => window.addEventListener('keydown', onKey))
onBeforeUnmount(() => window.removeEventListener('keydown', onKey))
</script>

<template>
  <header class="page-header">
    <div class="titles">
      <h1>{{ route.meta.title }}</h1>
      <p>{{ route.meta.subtitle }}</p>
    </div>
    <div class="tools">
      <label class="search">
        <Search :size="16" />
        <input ref="searchEl" v-model="ui.globalQuery" type="search" placeholder="搜索模型、提供商或请求 ID…" aria-label="全局搜索" />
        <kbd>{{ isMac ? '⌘' : 'Ctrl' }} K</kbd>
      </label>
      <button class="icon-btn" type="button" :aria-label="ui.theme === 'dark' ? '切换到浅色' : '切换到深色'" :disabled="settings.saving" @click="settings.toggleTheme()">
        <component :is="ui.theme === 'dark' ? Sun : Moon" :size="18" />
      </button>
      <span v-if="settings.themeError" class="theme-error" role="alert">{{ settings.themeError }}</span>
      <button class="icon-btn" type="button" aria-label="设置" @click="router.push('/settings')">
        <Settings :size="18" />
      </button>
    </div>
  </header>
</template>

<style scoped>
.page-header { display: flex; align-items: center; justify-content: space-between; gap: 16px; min-height: var(--header-h); padding: 0 var(--page-pad-x); border-bottom: 1px solid var(--c-border); flex: none; }
h1 { margin: 0; font-size: var(--fs-title); font-weight: 600; }
p { margin: 2px 0 0; color: var(--c-text-2); font-size: var(--fs-small); }
.tools { display: flex; align-items: center; gap: 10px; }
.search { display: flex; align-items: center; gap: 8px; width: 320px; height: var(--control-h); padding: 0 10px; border: 1px solid var(--c-border); border-radius: 8px; color: var(--c-text-2); }
.search input { flex: 1; min-width: 0; border: 0; outline: 0; background: transparent; }
.search:focus-within { border-color: var(--c-brand); }
kbd { padding: 1px 6px; border-radius: 5px; background: var(--c-surface-2); font: var(--fs-caption) var(--font); }
.icon-btn { display: grid; place-items: center; width: 36px; height: 36px; border: 1px solid var(--c-border); border-radius: 50%; background: var(--c-canvas); color: var(--c-text-2); }
.icon-btn:hover { background: var(--c-surface-2); }
.icon-btn:disabled { opacity: 0.5; cursor: not-allowed; }
.theme-error { max-width: 220px; color: var(--c-danger); font-size: var(--fs-caption); }
</style>
