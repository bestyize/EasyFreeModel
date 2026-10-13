import { defineStore } from 'pinia'
import { ref } from 'vue'

export const useUiStore = defineStore('ui', () => {
  const sidebarCollapsed = ref(false)
  const theme = ref<'light' | 'dark'>('light')
  /** 顶部全局搜索框内容，各页面按需读取做本页过滤 */
  const globalQuery = ref('')

  function toggleSidebar() {
    sidebarCollapsed.value = !sidebarCollapsed.value
  }

  // 首版优先浅色；深色仅切换 data-theme，样式由后续迭代补全
  function toggleTheme() {
    theme.value = theme.value === 'light' ? 'dark' : 'light'
    document.documentElement.dataset.theme = theme.value
  }

  return { sidebarCollapsed, theme, globalQuery, toggleSidebar, toggleTheme }
})
