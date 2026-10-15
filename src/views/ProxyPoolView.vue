<script setup lang="ts">
import { computed, onActivated, onMounted, ref } from 'vue'
import { ArrowDown, ArrowUp, FileInput, Gauge, Pencil, Plus, ShieldAlert, Trash2 } from 'lucide-vue-next'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import EmptyState from '@/components/EmptyState.vue'
import StatusTag from '@/components/StatusTag.vue'
import ProxyConfigCard from '@/components/proxy/ProxyConfigCard.vue'
import ProxyImportModal from '@/components/proxy/ProxyImportModal.vue'
import ProxyNodeModal from '@/components/proxy/ProxyNodeModal.vue'
import { nodeAddress, statusOf, strategyLabel } from '@/components/proxy/format'
import { errorMessage, formatDateTime } from '@/components/media/format'
import ToggleSwitch from '@/components/settings/ToggleSwitch.vue'
import { useProvidersStore } from '@/stores/providers'
import { useProxyPoolStore } from '@/stores/proxyPool'
import { useUiStore } from '@/stores/ui'
import type { ProxyNode } from '@/types'

const store = useProxyPoolStore()
const providers = useProvidersStore()
const ui = useUiStore()

const actionError = ref<string | null>(null)
const editing = ref<ProxyNode | null>(null)
const nodeModalOpen = ref(false)
const importOpen = ref(false)
const pendingDelete = ref<ProxyNode | null>(null)
const deleteOpen = computed({
  get: () => pendingDelete.value !== null,
  set: (v) => {
    if (!v) pendingDelete.value = null
  },
})

const filtering = computed(() => ui.globalQuery.trim() !== '')
const visibleNodes = computed(() => {
  const q = ui.globalQuery.trim().toLowerCase()
  if (!q) return store.nodes
  return store.nodes.filter((n) => n.name.toLowerCase().includes(q) || n.host.toLowerCase().includes(q) || (n.username ?? '').toLowerCase().includes(q))
})

const enabled = computed(() => store.saved?.enabled ?? false)
const noUsable = computed(() => enabled.value && store.usableCount === 0)
const scopeText = computed(() => {
  const c = store.saved
  if (!c) return ''
  if (c.providerScope === 'all') return '所有提供商'
  const n = c.providerIds.length
  return c.providerScope === 'include' ? `仅 ${n} 个指定提供商` : `除 ${n} 个提供商外`
})

async function run(label: string, fn: () => Promise<unknown>) {
  actionError.value = null
  try {
    await fn()
  } catch (e) {
    actionError.value = `${label}失败：${errorMessage(e)}`
  }
}

const toggleEnabled = (on: boolean) => run(on ? '启用代理池' : '关闭代理池', () => store.setEnabled(on))
const toggleNode = (n: ProxyNode, on: boolean) => run(`${on ? '启用' : '停用'}「${n.name}」`, () => store.setNodeEnabled(n.id, on))
const moveNode = (n: ProxyNode, delta: number) => run(`移动「${n.name}」`, () => store.move(n.id, delta))
const testNode = (n: ProxyNode) => run(`测速「${n.name}」`, () => store.test([n.id]))
const testAll = () => run('测速', () => store.testAll())

function openAdd() {
  editing.value = null
  nodeModalOpen.value = true
}
function openEdit(n: ProxyNode) {
  editing.value = n
  nodeModalOpen.value = true
}
async function confirmDelete() {
  const n = pendingDelete.value
  if (n) await run(`删除「${n.name}」`, () => store.remove(n.id))
}

function isBusy(id: string) {
  return store.busyIds.includes(id)
}

function statusDetail(n: ProxyNode): string | null {
  if (n.status === 'cooldown') return `${n.cooldownRemainingSeconds} 秒后重试`
  return null
}

let activatedOnce = false
function refresh() {
  void store.load()
  void providers.fetchProviders()
}
onMounted(refresh)
onActivated(() => {
  if (!activatedOnce) {
    activatedOnce = true
    return
  }
  refresh()
})
</script>

<template>
  <div class="page">
    <p v-if="store.loadError" class="card load-error" role="alert">
      <span>加载代理池失败：{{ store.loadError }}</span>
      <button class="btn" type="button" @click="store.load()">重试</button>
    </p>
    <p v-else-if="!store.loaded" class="loading">加载中…</p>

    <template v-else>
      <section class="card overview" aria-label="代理池总览">
        <div class="ov-text">
          <h2 class="section-title">启用代理池</h2>
          <p class="ov-desc">
            开启后，网关转发给上游的请求会按下面的策略经过代理节点；修改立即对新请求生效，无需重启网关。
          </p>
          <p class="ov-stats" data-testid="overview-stats">
            {{ store.nodes.length }} 个节点 · {{ store.usableCount }} 个可用 · {{ strategyLabel(store.saved!.strategy) }} · {{ scopeText }}
          </p>
        </div>
        <ToggleSwitch :model-value="enabled" label="启用代理池" data-testid="pool-toggle" @update:model-value="toggleEnabled" />
      </section>

      <div v-if="noUsable" class="banner" :class="store.saved!.fallbackDirect ? 'warn' : 'danger'" role="alert" data-testid="no-usable-banner">
        <ShieldAlert :size="18" aria-hidden="true" />
        <span v-if="store.saved!.fallbackDirect">代理池已开启，但目前没有可用节点，请求会回退为直连上游。</span>
        <span v-else>代理池已开启，但目前没有可用节点：生效范围内提供商的上游请求会被拒绝，直到添加并启用可用节点（或开启“无可用代理时回退直连”）。</span>
      </div>

      <ProxyConfigCard />

      <section class="card nodes" aria-labelledby="px-nodes">
        <header class="toolbar">
          <h2 id="px-nodes" class="section-title">代理节点</h2>
          <div class="tools">
            <button class="btn" type="button" data-testid="open-import" @click="importOpen = true">
              <FileInput :size="15" aria-hidden="true" /> 批量导入
            </button>
            <button class="btn" type="button" data-testid="test-all" :disabled="store.testingAll || store.nodes.length === 0" @click="testAll">
              <Gauge :size="15" aria-hidden="true" /> {{ store.testingAll ? '测速中…' : '全部测速' }}
            </button>
            <button class="btn btn-primary" type="button" data-testid="open-add" @click="openAdd">
              <Plus :size="16" aria-hidden="true" /> 添加节点
            </button>
          </div>
        </header>

        <p v-if="actionError" class="action-error" role="alert" data-testid="action-error">{{ actionError }}</p>

        <EmptyState v-if="store.nodes.length === 0" title="还没有代理节点" description="添加单个节点，或粘贴代理列表批量导入。支持 HTTP / HTTPS / SOCKS5。" dashed>
          <button class="btn" type="button" @click="importOpen = true">批量导入</button>
          <button class="btn btn-primary" type="button" @click="openAdd">添加节点</button>
        </EmptyState>
        <EmptyState v-else-if="visibleNodes.length === 0" title="没有匹配的节点" description="请调整顶部搜索关键字。" />

        <div v-else class="table-wrap">
          <table>
            <thead>
              <tr>
                <th class="order">顺序</th>
                <th>节点</th>
                <th>状态</th>
                <th>延迟 / 出口 IP</th>
                <th class="num">成功 / 失败</th>
                <th>启用</th>
                <th>操作</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="(n, i) in visibleNodes" :key="n.id" :data-testid="`row-${n.id}`">
                <td class="order">
                  <button class="mini" type="button" :disabled="filtering || i === 0 || isBusy(n.id)" :aria-label="`上移 ${n.name}`" :data-testid="`up-${n.id}`" @click="moveNode(n, -1)">
                    <ArrowUp :size="13" aria-hidden="true" />
                  </button>
                  <button class="mini" type="button" :disabled="filtering || i === visibleNodes.length - 1 || isBusy(n.id)" :aria-label="`下移 ${n.name}`" :data-testid="`down-${n.id}`" @click="moveNode(n, 1)">
                    <ArrowDown :size="13" aria-hidden="true" />
                  </button>
                </td>
                <td>
                  <div class="name">{{ n.name }}</div>
                  <div class="sub mono">
                    {{ nodeAddress(n) }}<template v-if="n.username"> · {{ n.username }}<template v-if="n.hasPassword"> · ••••</template></template>
                  </div>
                </td>
                <td>
                  <StatusTag :tone="statusOf(n).tone">{{ statusOf(n).label }}</StatusTag>
                  <div v-if="statusDetail(n)" class="sub">{{ statusDetail(n) }}</div>
                  <div v-if="n.lastError && n.status !== 'healthy'" class="sub err" :title="n.lastError">{{ n.lastError }}</div>
                </td>
                <td>
                  <div v-if="n.latencyMs != null">{{ n.latencyMs }} ms</div>
                  <div v-else class="sub">—</div>
                  <div v-if="n.exitIp" class="sub mono" :title="`测速于 ${formatDateTime(n.lastCheckedAt)}`">{{ n.exitIp }}</div>
                </td>
                <td class="num">{{ n.successCount.toLocaleString() }} / {{ n.failureCount.toLocaleString() }}</td>
                <td>
                  <ToggleSwitch :model-value="n.enabled" :disabled="isBusy(n.id)" :label="`${n.enabled ? '停用' : '启用'} ${n.name}`" :data-testid="`toggle-${n.id}`" @update:model-value="toggleNode(n, $event)" />
                </td>
                <td>
                  <div class="actions">
                    <button class="btn" type="button" :disabled="store.testingIds.includes(n.id) || store.testingAll" :data-testid="`test-${n.id}`" @click="testNode(n)">
                      {{ store.testingIds.includes(n.id) ? '测速中…' : '测速' }}
                    </button>
                    <button class="icon" type="button" :aria-label="`编辑 ${n.name}`" title="编辑" :data-testid="`edit-${n.id}`" @click="openEdit(n)">
                      <Pencil :size="16" aria-hidden="true" />
                    </button>
                    <button class="icon danger" type="button" :disabled="isBusy(n.id)" :aria-label="`删除 ${n.name}`" title="删除" :data-testid="`delete-${n.id}`" @click="pendingDelete = n">
                      <Trash2 :size="16" aria-hidden="true" />
                    </button>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>
    </template>

    <ProxyNodeModal v-model="nodeModalOpen" :node="editing" />
    <ProxyImportModal v-model="importOpen" />
    <ConfirmDialog
      v-model="deleteOpen"
      title="删除代理节点"
      :message="`删除「${pendingDelete?.name}」后无法恢复，已保存的密码也会一并清除。`"
      confirm-text="确认删除"
      danger
      @confirm="confirmDelete"
    />
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 16px; }
.loading { margin: 0; color: var(--c-text-2); }
.load-error { display: flex; align-items: center; justify-content: space-between; margin: 0; padding: 14px 16px; color: var(--c-danger); }
.overview { display: flex; align-items: center; justify-content: space-between; gap: 24px; padding: 18px 24px; }
.ov-text { min-width: 0; }
.ov-desc { margin: 4px 0 0; color: var(--c-text-2); font-size: var(--fs-small); line-height: 1.6; }
.ov-stats { margin: 8px 0 0; font-size: var(--fs-small); font-weight: 500; }
.banner { display: flex; align-items: flex-start; gap: 10px; padding: 12px 14px; border-radius: var(--radius); font-size: var(--fs-small); line-height: 1.6; }
.banner svg { flex: none; margin-top: 2px; }
.banner.danger { border: 1px solid var(--c-danger-border); background: var(--c-danger-soft); color: var(--c-danger); }
.banner.warn { border: 1px solid var(--c-warning-border); background: var(--c-warning-soft); color: var(--c-warning); }
.nodes { padding: 18px 0 0; overflow: hidden; }
.toolbar { display: flex; align-items: center; justify-content: space-between; padding: 0 24px 14px; }
.tools { display: flex; gap: 8px; }
.action-error { margin: 0 24px 12px; padding: 10px 12px; border-radius: 8px; background: var(--c-danger-soft); color: var(--c-danger); font-size: var(--fs-small); }
.nodes > :deep(.empty-state) { margin: 0 24px 24px; }
.table-wrap { overflow-x: auto; border-top: 1px solid var(--c-border); }
table { width: 100%; border-collapse: collapse; font-size: var(--fs-small); }
th { padding: 10px 14px; text-align: left; color: var(--c-text-2); font-weight: 500; background: var(--c-sidebar); border-bottom: 1px solid var(--c-border); white-space: nowrap; }
td { padding: 12px 14px; border-bottom: 1px solid var(--c-border); vertical-align: middle; }
tbody tr:last-child td { border-bottom: 0; }
.order { width: 64px; white-space: nowrap; }
.num { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
.name { font-weight: 500; }
.sub { margin-top: 2px; color: var(--c-text-2); font-size: var(--fs-caption); }
.sub.err { max-width: 260px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--c-danger); }
.mono { font-family: var(--font-mono); }
.mini { display: inline-grid; place-items: center; width: 24px; height: 24px; border: 1px solid var(--c-border); border-radius: 6px; background: var(--c-canvas); color: var(--c-text-2); padding: 0; }
.mini + .mini { margin-left: 4px; }
.mini:hover:not(:disabled) { background: var(--c-surface-2); color: var(--c-text); }
.mini:disabled { opacity: 0.35; cursor: not-allowed; }
.actions { display: flex; align-items: center; gap: 6px; }
.icon { display: grid; place-items: center; width: 32px; height: 32px; border: 1px solid var(--c-border); border-radius: 8px; background: var(--c-canvas); color: var(--c-text-2); }
.icon:hover { background: var(--c-surface-2); }
.icon.danger { color: var(--c-danger); }
.icon:disabled { opacity: 0.5; cursor: not-allowed; }
</style>
