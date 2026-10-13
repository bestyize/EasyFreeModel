<script setup lang="ts">
import { computed } from 'vue'
import { RouterLink } from 'vue-router'
import { PanelLeftClose, PanelLeftOpen, Waypoints } from 'lucide-vue-next'
import { APP_NAME, APP_VERSION, NAV_ITEMS } from '@/router/nav'
import { useSystemStore } from '@/stores/system'

defineProps<{ collapsed: boolean }>()
defineEmits<{ toggle: [] }>()

const system = useSystemStore()
const running = computed(() => system.status?.gateway.running ?? false)
const stateText = computed(() => {
  if (!system.status) return system.loadError ? '状态未知' : '检测中'
  if (system.status.gateway.error) return '异常'
  return running.value ? '运行中' : '已停止'
})
const stateTone = computed(() => {
  if (!system.status || system.status.gateway.error) return 'var(--c-danger)'
  return running.value ? 'var(--c-success)' : 'var(--c-text-2)'
})
const fmt = (n: number | null | undefined, suffix = '') => (n == null ? '—' : `${n.toLocaleString()}${suffix}`)
</script>

<template>
  <aside class="sidebar" :class="{ collapsed }" aria-label="主导航">
    <div class="brand">
      <span class="logo"><Waypoints :size="20" color="#fff" /></span>
      <div v-if="!collapsed" class="brand-text">
        <strong>{{ APP_NAME }}</strong>
        <small>v{{ APP_VERSION }}</small>
      </div>
    </div>

    <nav class="nav">
      <RouterLink
        v-for="item in NAV_ITEMS"
        :key="item.path"
        :to="item.path"
        class="nav-item"
        active-class="active"
        :title="collapsed ? item.label : undefined"
      >
        <component :is="item.icon" :size="19" />
        <span v-if="!collapsed">{{ item.label }}</span>
      </RouterLink>
    </nav>

    <div class="bottom">
      <section v-if="!collapsed" class="status card" aria-label="本地服务状态">
        <header>
          <strong>系统状态</strong>
          <span :style="{ color: stateTone }"><i class="dot" :style="{ background: stateTone }" />{{ stateText }}</span>
        </header>
        <dl>
          <div><dt>API 请求</dt><dd>{{ fmt(system.status?.apiRequests) }}</dd></div>
          <div><dt>成功率</dt><dd>{{ fmt(system.status?.successRate, '%') }}</dd></div>
          <div><dt>平均延迟</dt><dd>{{ fmt(system.status?.avgLatencyMs, 'ms') }}</dd></div>
        </dl>
      </section>
      <i v-else class="dot solo" :style="{ background: stateTone }" :title="stateText" />
      <button class="collapse-btn" type="button" :aria-label="collapsed ? '展开侧栏' : '折叠侧栏'" @click="$emit('toggle')">
        <component :is="collapsed ? PanelLeftOpen : PanelLeftClose" :size="18" />
      </button>
    </div>
  </aside>
</template>

<style scoped>
.sidebar {
  display: flex;
  flex-direction: column;
  width: var(--sidebar-w);
  flex: none;
  background: var(--c-sidebar);
  border-right: 1px solid var(--c-border);
  padding: 16px 12px;
  transition: width 0.15s ease;
}
.sidebar.collapsed { width: var(--sidebar-w-collapsed); }
.brand { display: flex; align-items: center; gap: 10px; padding: 4px 6px 18px; }
.logo { display: grid; place-items: center; width: 36px; height: 36px; border-radius: 9px; background: var(--c-brand); flex: none; }
.brand-text { display: flex; flex-direction: column; line-height: 1.25; }
.brand-text small { color: var(--c-text-2); font-size: var(--fs-caption); }
.nav { display: flex; flex-direction: column; gap: 4px; }
.nav-item { display: flex; align-items: center; gap: 12px; height: 40px; padding: 0 12px; border-radius: 9px; color: var(--c-text-2); text-decoration: none; }
.collapsed .nav-item { justify-content: center; padding: 0; }
.nav-item:hover { background: var(--c-surface-2); }
.nav-item.active { background: var(--c-brand-soft); color: var(--c-brand); font-weight: 600; }
.bottom { margin-top: auto; display: flex; flex-direction: column; gap: 10px; align-items: stretch; }
.collapsed .bottom { align-items: center; }
.status { padding: 12px; font-size: var(--fs-small); }
.status header { display: flex; justify-content: space-between; margin-bottom: 8px; }
.status header span { display: inline-flex; align-items: center; gap: 5px; font-size: var(--fs-caption); }
.dot { width: 7px; height: 7px; border-radius: 50%; display: inline-block; }
.dot.solo { width: 9px; height: 9px; }
.status dl { margin: 0; display: grid; gap: 6px; }
.status dl div { display: flex; justify-content: space-between; }
.status dt { color: var(--c-text-2); }
.status dd { margin: 0; font-weight: 600; }
.collapse-btn { display: grid; place-items: center; width: 32px; height: 32px; border: 0; border-radius: 8px; background: transparent; color: var(--c-text-2); }
.collapse-btn:hover { background: var(--c-surface-2); }
</style>
