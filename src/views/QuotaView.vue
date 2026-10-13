<script setup lang="ts">
import { computed, onActivated, onBeforeUnmount, onDeactivated, onMounted, ref } from 'vue'
import { CircleCheck, CircleOff, Filter, LayoutGrid, RefreshCw, User, X } from 'lucide-vue-next'
import EmptyState from '@/components/EmptyState.vue'
import QuotaCard from '@/components/quota/QuotaCard.vue'
import ToggleSwitch from '@/components/quota/ToggleSwitch.vue'
import { useQuotaStore, type QuotaSort } from '@/stores/quota'

/** 账户很多时分批渲染，避免一次挂载几百张卡片 */
const PAGE_SIZE = 20

const store = useQuotaStore()
const limit = ref(PAGE_SIZE)
const now = ref(Date.now())
let nowTimer: ReturnType<typeof setInterval> | undefined

const shown = computed(() => store.visible.slice(0, limit.value))
const remaining = computed(() => Math.max(0, store.visible.length - limit.value))

function startNow() {
  stopNow()
  now.value = Date.now()
  // 倒计时精度到分钟，30 秒刷新一次足够
  nowTimer = setInterval(() => (now.value = Date.now()), 30_000)
}
function stopNow() {
  if (nowTimer !== undefined) clearInterval(nowTimer)
  nowTimer = undefined
}

function onProvider(e: Event) {
  store.setProviderFilter((e.target as HTMLSelectElement).value)
  limit.value = PAGE_SIZE
}
function onAccount(e: Event) {
  store.accountFilter = (e.target as HTMLSelectElement).value
  limit.value = PAGE_SIZE
}
function onSort(e: Event) {
  store.sort = (e.target as HTMLSelectElement).value as QuotaSort
}

function start() {
  startNow()
  store.resume()
}
function stop() {
  stopNow()
  store.suspend()
}

onMounted(() => {
  start()
  void store.load()
})
let activatedOnce = false
onActivated(() => {
  if (!activatedOnce) {
    activatedOnce = true
    return
  }
  start()
  void store.load()
})
onDeactivated(stop)
onBeforeUnmount(stop)
</script>

<template>
  <div class="quota-page">
    <div class="toolbar" role="toolbar" aria-label="配额筛选与操作">
      <label class="select">
        <LayoutGrid :size="15" aria-hidden="true" />
        <select aria-label="提供商筛选" :value="store.providerFilter" @change="onProvider">
          <option value="">所有提供商</option>
          <option v-for="p in store.providerOptions" :key="p.id" :value="p.id">{{ p.name }}</option>
        </select>
      </label>
      <label class="select">
        <User :size="15" aria-hidden="true" />
        <select aria-label="账号筛选" :value="store.accountFilter" @change="onAccount">
          <option value="">所有账号</option>
          <option v-for="a in store.accountOptions" :key="a.id" :value="a.id">{{ a.label }}</option>
        </select>
      </label>
      <label class="select">
        <Filter :size="15" aria-hidden="true" />
        <select aria-label="排序" :value="store.sort" @change="onSort">
          <option value="expiring">即将过期优先</option>
          <option value="default">默认顺序</option>
        </select>
      </label>
      <span class="divider" aria-hidden="true" />
      <button class="btn danger" type="button" data-test="close-empty" @click="store.closeEmptyAccounts()">
        <CircleOff :size="14" />关闭空账号
      </button>
      <button class="btn success" type="button" data-test="enable-available" @click="store.enableAvailableAccounts()">
        <CircleCheck :size="14" />开启可用账号
      </button>
      <span class="divider" aria-hidden="true" />
      <label class="auto">
        <ToggleSwitch :model-value="store.autoRefresh" label="自动刷新" @change="store.setAutoRefresh" />
        <span>自动刷新<template v-if="store.autoRefresh"> ({{ store.countdown }}s)</template></span>
      </label>
      <button class="btn refresh" type="button" :disabled="store.refreshing" aria-label="刷新全部配额" data-test="refresh" @click="store.refreshAll()">
        <RefreshCw :size="15" :class="{ spin: store.refreshing }" />
      </button>
    </div>

    <p v-if="store.notice" class="notice" role="status">
      {{ store.notice }}
      <button type="button" aria-label="关闭提示" @click="store.dismissNotice()"><X :size="14" /></button>
    </p>
    <p v-if="store.refreshError" class="banner" role="alert">
      刷新失败：{{ store.refreshError }}（显示的是上次同步的数据）
      <button class="btn" type="button" @click="store.refreshAll()">重试</button>
    </p>

    <p v-if="store.status === 'loading' && store.accounts.length === 0" class="state" role="status">加载中…</p>
    <EmptyState v-else-if="store.status === 'error' && store.accounts.length === 0" title="配额加载失败" :description="store.loadError ?? ''" dashed>
      <button class="btn btn-primary" type="button" @click="store.load()">重试</button>
    </EmptyState>
    <EmptyState v-else-if="store.accounts.length === 0" title="暂无配额账户" description="在「提供商」页面添加并连接账户后，这里会显示各账户的额度。" dashed />
    <EmptyState v-else-if="store.visible.length === 0" title="没有符合条件的账户" dashed>
      <button class="btn" type="button" @click="store.resetFilters()">重置筛选</button>
    </EmptyState>
    <template v-else>
      <div class="grid">
        <QuotaCard
          v-for="a in shown"
          :key="a.connectionId"
          :account="a"
          :now="now"
          :syncing="store.syncingIds.includes(a.connectionId)"
          :toggling="store.togglingIds.includes(a.connectionId)"
          @sync="store.refreshOne"
          @toggle="(id, v) => store.setEnabled(id, v)"
        />
      </div>
      <button v-if="remaining > 0" class="btn more" type="button" @click="limit += PAGE_SIZE">显示更多（还有 {{ remaining }} 个账户）</button>
    </template>
  </div>
</template>

<style scoped>
.quota-page { display: flex; flex-direction: column; gap: var(--card-gap); }
.toolbar { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
.select { display: inline-flex; align-items: center; gap: 6px; height: var(--control-h); padding: 0 8px 0 10px; border: 1px solid var(--c-border); border-radius: 8px; color: var(--c-text-2); background: var(--c-canvas); }
.select select { max-width: 200px; border: 0; outline: 0; background: transparent; font-size: var(--fs-small); color: var(--c-text); }
.select:focus-within { border-color: var(--c-brand); }
.divider { width: 1px; height: 22px; background: var(--c-border); }
.btn.danger { border-color: #f6c9c9; background: #fdecec; color: var(--c-danger); }
.btn.success { border-color: #bfe6d5; background: #e8f6f0; color: var(--c-success); }
.auto { display: inline-flex; align-items: center; gap: 8px; height: var(--control-h); padding: 0 12px; border: 1px solid var(--c-border); border-radius: 8px; font-size: var(--fs-small); font-variant-numeric: tabular-nums; }
.refresh { width: var(--control-h); padding: 0; }
.spin { animation: spin 0.9s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(380px, 1fr)); gap: var(--card-gap); align-items: start; }
.more { align-self: center; }
.state { margin: 0; padding: 60px 0; text-align: center; color: var(--c-text-2); }
.notice { display: flex; align-items: center; justify-content: space-between; margin: 0; padding: 8px 12px; border-radius: 8px; background: var(--c-surface-2); font-size: var(--fs-small); }
.notice button { display: grid; place-items: center; border: 0; background: transparent; color: var(--c-text-2); }
.banner { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin: 0; padding: 8px 12px; border-radius: 8px; background: #fdecec; color: var(--c-danger); font-size: var(--fs-small); }
</style>
