<script setup lang="ts">
import { computed, onActivated, onMounted, ref } from 'vue'
import { Copy, Eye, EyeOff, Link2, Plus, RotateCw, ShieldAlert, Trash2 } from 'lucide-vue-next'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import EmptyState from '@/components/EmptyState.vue'
import StatusTag from '@/components/StatusTag.vue'
import ApiKeyCreateModal from '@/components/apikeys/ApiKeyCreateModal.vue'
import KeyImportFlow from '@/components/guide/KeyImportFlow.vue'
import SecretRevealModal from '@/components/apikeys/SecretRevealModal.vue'
import { isLoopbackAddress } from '@/components/settings/address'
import { protocolLabel } from '@/components/settings/protocols'
import { errorMessage, formatDateTime } from '@/components/media/format'
import { useApiKeysStore } from '@/stores/apiKeys'
import { useSettingsStore } from '@/stores/settings'
import { useSystemStore } from '@/stores/system'
import { useUiStore } from '@/stores/ui'
import type { ApiKeyInfo, ApiKeySecretResult } from '@/types'

const store = useApiKeysStore()
const settings = useSettingsStore()
const system = useSystemStore()
const ui = useUiStore()

const createOpen = ref(false)
const importKey = ref<ApiKeyInfo | null>(null)
const importOpen = computed({
  get: () => importKey.value !== null,
  set: (v: boolean) => { if (!v) importKey.value = null },
})
const actionError = ref<string | null>(null)

/** 用于创建、轮换及查看密钥弹窗展示 */
const revealed = ref<{ name: string; secret: string; mode: 'create' | 'rotate' | 'view' } | null>(null)
const revealOpen = computed({
  get: () => revealed.value !== null,
  set: (v) => {
    if (!v) revealed.value = null
  },
})

const showPlainMap = ref<Record<string, boolean>>({})
const copiedId = ref<string | null>(null)

function toggleShowPlain(id: string) {
  showPlainMap.value[id] = !showPlainMap.value[id]
}

function viewKey(k: ApiKeyInfo) {
  revealed.value = { name: k.name, secret: k.secret || k.masked, mode: 'view' }
}

async function copyKey(k: ApiKeyInfo) {
  const text = k.secret || k.masked
  try {
    await navigator.clipboard.writeText(text)
    copiedId.value = k.id
    setTimeout(() => {
      if (copiedId.value === k.id) copiedId.value = null
    }, 1500)
  } catch (e) {
    actionError.value = `复制失败：${errorMessage(e)}`
  }
}

const pending = ref<{ kind: 'rotate' | 'delete'; key: ApiKeyInfo } | null>(null)
const confirmOpen = computed({
  get: () => pending.value !== null,
  set: (v) => {
    if (!v) pending.value = null
  },
})

const exposed = computed(() => {
  const configured = settings.saved?.listenAddress
  const running = system.status?.gateway.running ? system.status.gateway.address : null
  return (configured != null && !isLoopbackAddress(configured)) || (running != null && !isLoopbackAddress(running))
})

const visibleKeys = computed(() => {
  const q = ui.globalQuery.trim().toLowerCase()
  if (!q) return store.keys
  return store.keys.filter((k) => k.name.toLowerCase().includes(q) || k.masked.toLowerCase().includes(q))
})

function isExpired(k: ApiKeyInfo): boolean {
  return !!k.expiresAt && new Date(k.expiresAt).getTime() <= Date.now()
}

function statusOf(k: ApiKeyInfo): { label: string; tone: 'success' | 'neutral' | 'danger' } {
  if (k.disabled) return { label: '已停用', tone: 'neutral' }
  if (isExpired(k)) return { label: '已过期', tone: 'danger' }
  return { label: '启用中', tone: 'success' }
}

function scopeText(k: ApiKeyInfo): { protocols: string; models: string; limits: string } {
  const limits = [
    k.rpmLimit != null ? `RPM ${k.rpmLimit}` : null,
    k.tpmLimit != null ? `TPM ${k.tpmLimit}` : null,
    k.totalLimit != null ? `累计 ${k.totalLimit}` : null,
  ].filter(Boolean)
  return {
    protocols: k.protocols.map(protocolLabel).join('、') || '无协议',
    models: k.allowedModels.length ? `${k.allowedModels.length} 个模型` : '全部模型',
    limits: limits.length ? limits.join(' · ') : '无限额',
  }
}

function canImport(k: ApiKeyInfo): boolean {
  return !!k.secret && !k.disabled && !isExpired(k)
}

function isBusy(id: string): boolean {
  return store.busyIds.includes(id)
}

function onCreated(result: ApiKeySecretResult) {
  actionError.value = null
  revealed.value = { name: result.key.name, secret: result.secret, mode: 'create' }
}

async function toggleDisabled(k: ApiKeyInfo) {
  actionError.value = null
  try {
    await store.setDisabled(k.id, !k.disabled)
  } catch (e) {
    actionError.value = `${k.disabled ? '启用' : '停用'}「${k.name}」失败：${errorMessage(e)}`
  }
}

async function onConfirm() {
  const target = pending.value
  if (!target) return
  actionError.value = null
  const { kind, key } = target
  try {
    if (kind === 'rotate') {
      const result = await store.rotate(key.id)
      revealed.value = { name: result.key.name, secret: result.secret, mode: 'rotate' }
    } else {
      await store.remove(key.id)
    }
  } catch (e) {
    actionError.value = `${kind === 'rotate' ? '轮换' : '删除'}「${key.name}」失败：${errorMessage(e)}`
  }
}

let activatedOnce = false
onMounted(() => {
  void store.load()
  void settings.ensureLoaded()
})
onActivated(() => {
  // KeepAlive 首次激活紧随 mounted，已加载过，跳过
  if (!activatedOnce) {
    activatedOnce = true
    return
  }
  void store.load()
  void settings.ensureLoaded()
})
</script>

<template>
  <div class="page">
    <div v-if="exposed" class="risk" role="alert" data-testid="risk-banner">
      <ShieldAlert :size="18" aria-hidden="true" />
      <span>
        本地服务未仅绑定回环地址（127.0.0.1 / localhost），局域网或外网设备可能访问到网关。
        请务必启用密钥认证、限制协议与模型范围，并避免将密钥发给不受信任的人。
      </span>
    </div>

    <div class="toolbar">
      <span class="count">共 {{ store.keys.length }} 个密钥</span>
      <button class="btn btn-primary" type="button" data-testid="open-create" @click="createOpen = true">
        <Plus :size="16" aria-hidden="true" /> 创建密钥
      </button>
    </div>

    <p v-if="actionError" class="action-error" role="alert" data-testid="action-error">{{ actionError }}</p>

    <div v-if="store.loadError" class="card load-error" role="alert">
      <span>加载密钥列表失败：{{ store.loadError }}</span>
      <button class="btn" type="button" @click="store.load()">重试</button>
    </div>
    <p v-else-if="store.loading && !store.loaded" class="loading">加载中…</p>
    <EmptyState v-else-if="store.keys.length === 0" title="还没有密钥" description="创建密钥后，客户端即可使用它访问本地网关。" dashed>
      <button class="btn btn-primary" type="button" @click="createOpen = true">创建密钥</button>
    </EmptyState>
    <EmptyState v-else-if="visibleKeys.length === 0" title="没有匹配的密钥" description="请调整顶部搜索关键字。" dashed />

    <div v-else class="card table-wrap">
      <table>
        <thead>
          <tr>
            <th>名称</th>
            <th>密钥</th>
            <th>权限范围</th>
            <th class="num">调用次数</th>
            <th>创建时间</th>
            <th>最后使用</th>
            <th>状态</th>
            <th>操作</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="k in visibleKeys" :key="k.id" :data-testid="`row-${k.id}`">
            <td class="name">
              {{ k.name }}
              <div v-if="k.expiresAt" class="sub">到期 {{ formatDateTime(k.expiresAt) }}</div>
            </td>
            <td>
              <div class="key-cell">
                <code class="mask" :title="k.secret || k.masked" :data-testid="`key-value-${k.id}`">
                  {{ showPlainMap[k.id] && k.secret ? k.secret : k.masked }}
                </code>
                <button
                  v-if="k.secret"
                  class="mini-btn"
                  type="button"
                  :title="showPlainMap[k.id] ? '隐藏完整密钥' : '显示完整密钥'"
                  :aria-label="showPlainMap[k.id] ? '隐藏完整密钥' : '显示完整密钥'"
                  :data-testid="`toggle-plain-${k.id}`"
                  @click="toggleShowPlain(k.id)"
                >
                  <component :is="showPlainMap[k.id] ? EyeOff : Eye" :size="14" aria-hidden="true" />
                </button>
                <button
                  class="mini-btn"
                  type="button"
                  :title="copiedId === k.id ? '已复制' : '复制密钥'"
                  :aria-label="copiedId === k.id ? '已复制' : '复制密钥'"
                  :data-testid="`copy-${k.id}`"
                  @click="copyKey(k)"
                >
                  <Copy :size="14" aria-hidden="true" />
                </button>
              </div>
            </td>
            <td class="scope">
              <div :title="k.allowedModels.join('\n') || undefined">{{ scopeText(k).protocols }}</div>
              <div class="sub">{{ scopeText(k).models }} · {{ scopeText(k).limits }}</div>
            </td>
            <td class="num">{{ k.callCount.toLocaleString() }}</td>
            <td>{{ formatDateTime(k.createdAt) }}</td>
            <td>{{ formatDateTime(k.lastUsedAt, '从未使用') }}</td>
            <td><StatusTag :tone="statusOf(k).tone">{{ statusOf(k).label }}</StatusTag></td>
            <td>
              <div class="actions">
                <button class="btn import-btn" type="button" :disabled="isBusy(k.id) || !canImport(k)" :title="canImport(k) ? '选择要接入的客户端' : '需要启用中的完整密钥'" :data-testid="'import-' + k.id" @click="importKey = k">
                  <Link2 :size="15" aria-hidden="true" /> 接入客户端
                </button>
                <button
                  class="icon" type="button" :disabled="isBusy(k.id)" :aria-label="`查看 ${k.name}`" title="查看完整密钥"
                  :data-testid="`view-${k.id}`" @click="viewKey(k)"
                >
                  <Eye :size="16" aria-hidden="true" />
                </button>
                <button class="btn" type="button" :disabled="isBusy(k.id)" :data-testid="`toggle-${k.id}`" @click="toggleDisabled(k)">
                  {{ k.disabled ? '启用' : '停用' }}
                </button>
                <button
                  class="icon" type="button" :disabled="isBusy(k.id)" :aria-label="`轮换 ${k.name}`" title="轮换"
                  :data-testid="`rotate-${k.id}`" @click="pending = { kind: 'rotate', key: k }"
                >
                  <RotateCw :size="16" aria-hidden="true" />
                </button>
                <button
                  class="icon danger" type="button" :disabled="isBusy(k.id)" :aria-label="`删除 ${k.name}`" title="删除"
                  :data-testid="`delete-${k.id}`" @click="pending = { kind: 'delete', key: k }"
                >
                  <Trash2 :size="16" aria-hidden="true" />
                </button>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <KeyImportFlow v-model="importOpen" :api-key="importKey" />

    <ApiKeyCreateModal v-model="createOpen" @created="onCreated" />

    <SecretRevealModal
      v-model="revealOpen"
      :secret="revealed?.secret ?? ''"
      :key-name="revealed?.name ?? ''"
      :mode="revealed?.mode ?? 'create'"
    />

    <ConfirmDialog
      v-model="confirmOpen"
      :title="pending?.kind === 'rotate' ? '轮换密钥' : '删除密钥'"
      :message="
        pending?.kind === 'rotate'
          ? `轮换「${pending?.key.name}」后旧密钥会立即失效，所有正在使用它的客户端都需要换成新密钥。新密钥将保存并随时可查看。`
          : `删除「${pending?.key.name}」后无法恢复，使用它的客户端将立即无法访问。`
      "
      :confirm-text="pending?.kind === 'rotate' ? '确认轮换' : '确认删除'"
      danger
      @confirm="onConfirm"
    />
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 16px; }
.risk { display: flex; align-items: flex-start; gap: 10px; padding: 12px 14px; border: 1px solid #f3c9c9; border-radius: var(--radius); background: #fdecec; color: #a63232; font-size: var(--fs-small); line-height: 1.6; }
.risk svg { flex: none; margin-top: 2px; }
.toolbar { display: flex; align-items: center; justify-content: space-between; }
.count { color: var(--c-text-2); font-size: var(--fs-small); }
.action-error { margin: 0; padding: 10px 12px; border-radius: 8px; background: #fdecec; color: var(--c-danger); font-size: var(--fs-small); }
.load-error { display: flex; align-items: center; justify-content: space-between; padding: 14px 16px; color: var(--c-danger); }
.loading { margin: 0; color: var(--c-text-2); }
.table-wrap { overflow-x: auto; }
table { width: 100%; border-collapse: collapse; font-size: var(--fs-small); }
th { padding: 10px 14px; text-align: left; color: var(--c-text-2); font-weight: 500; background: var(--c-sidebar); border-bottom: 1px solid var(--c-border); white-space: nowrap; }
td { padding: 12px 14px; border-bottom: 1px solid var(--c-border); vertical-align: middle; }
tbody tr:last-child td { border-bottom: 0; }
.num { text-align: right; font-variant-numeric: tabular-nums; }
.name { font-weight: 500; }
.sub { margin-top: 2px; color: var(--c-text-2); font-size: var(--fs-caption); font-weight: 400; }
.mask { font-family: var(--font-mono); }
.key-cell { display: inline-flex; align-items: center; gap: 6px; }
.mini-btn { display: grid; place-items: center; width: 26px; height: 26px; border: 1px solid var(--c-border); border-radius: 6px; background: var(--c-canvas); color: var(--c-text-2); padding: 0; cursor: pointer; }
.mini-btn:hover { background: var(--c-surface-2); color: var(--c-text); }
.scope { max-width: 280px; }
.actions { display: flex; align-items: center; gap: 6px; white-space: nowrap; }
.import-btn { display: inline-flex; align-items: center; gap: 5px; color: var(--c-brand); }
.import-btn:disabled { opacity: 0.5; }
.icon { display: grid; place-items: center; width: 32px; height: 32px; border: 1px solid var(--c-border); border-radius: 8px; background: var(--c-canvas); color: var(--c-text-2); }
.icon:hover { background: var(--c-surface-2); }
.icon.danger { color: var(--c-danger); }
.icon:disabled { opacity: 0.5; cursor: not-allowed; }
</style>
