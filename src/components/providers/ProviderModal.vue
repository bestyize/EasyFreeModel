<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { AlertTriangle, ExternalLink, Lock, Plus, Trash2, Zap } from 'lucide-vue-next'
import BaseModal from '@/components/BaseModal.vue'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import StatusTag from '@/components/StatusTag.vue'
import ConnectionRow from './ConnectionRow.vue'
import HeadersEditor from './HeadersEditor.vue'
import ModelList from './ModelList.vue'
import OAuthPanel from './oauth/OAuthPanel.vue'
import ProviderLogo from './ProviderLogo.vue'
import ToggleSwitch from './ToggleSwitch.vue'
import {
  cloneDetail,
  conflictModelIds,
  findAliasConflicts,
  isDetailDirty,
  toSavePayload,
  validateDetail,
} from './draft'
import { protocolText, providerStatus } from './status'
import { validateAccountLabel, validateApiKey } from './validation'
import { oauthErrorText, toOAuthApiError } from '@/api/oauthError'
import { useOAuthStore } from '@/stores/oauth'
import { useProvidersStore } from '@/stores/providers'
import type { Connection, ProviderDetail, ProviderSummary } from '@/types'

const props = defineProps<{
  modelValue: boolean
  /** 打开的提供商（摘要，用于加载期间展示头部） */
  provider: ProviderSummary | null
}>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean]; saved: [detail: ProviderDetail] }>()

const store = useProvidersStore()
const oauthStore = useOAuthStore()

const modalRef = ref<InstanceType<typeof BaseModal> | null>(null)
const loading = ref(false)
const loadError = ref<string | null>(null)
const original = ref<ProviderDetail | null>(null)
const draft = ref<ProviderDetail | null>(null)
const saving = ref(false)
const saveError = ref<string | null>(null)
const showIssues = ref(false)

const selected = ref<Set<string>>(new Set())
const expanded = ref<Set<string>>(new Set())
const concurrentTest = ref(false)
const parallel = ref(1)
const batchTesting = ref(false)
const pendingDelete = ref<Connection | null>(null)
const confirmDeleteOpen = ref(false)
let loadToken = 0

// 模型级探测的就地提示（结果表本身在 store 里，key = modelId）。
// 必须在下面的 watch(immediate) 之前声明：它会立刻调用 load()。
const modelTestNote = ref<string | null>(null)
const modelTestOk = ref(false)

// 添加 API Key 连接的内嵌表单。apiKey 只存在于这个输入框的本地 ref：
// 提交的瞬间就清空，不进入 store、不写本地存储、不打日志；关闭弹窗/卸载时同样清空。
const addOpen = ref(false)
const addLabel = ref('')
const addKey = ref('')
const adding = ref(false)
const addError = ref<string | null>(null)
const addNotice = ref<string | null>(null)
// 更换 API Key 的状态（按连接 id）；同样不保存密钥本身
const credBusy = ref<Set<string>>(new Set())
const credMsg = ref<Record<string, { ok: boolean; text: string }>>({})
// OAuth 授权：面板内嵌在“连接”区域；重新授权时带上目标连接。
// 令牌只在 Rust 侧，这里只有面板状态、提示文案和待确认断开的连接（元数据）。
const oauthPanelOpen = ref(false)
const reauthTarget = ref<Connection | null>(null)
const oauthPanelKey = ref(0)
const oauthNotice = ref<string | null>(null)
const oauthError = ref<string | null>(null)
const oauthBusyIds = ref<Set<string>>(new Set())
const pendingDisconnect = ref<Connection | null>(null)
const confirmDisconnectOpen = ref(false)
// 删除自定义提供商
const confirmDeleteProviderOpen = ref(false)
const deletingProvider = ref(false)

const readonly = computed(() => !draft.value?.adapterReady || saving.value)

function sortedByPriority(d: ProviderDetail): ProviderDetail {
  d.connections = [...d.connections].sort((a, b) => a.priority - b.priority)
  return d
}

async function load() {
  const token = ++loadToken
  loadError.value = null
  saveError.value = null
  showIssues.value = false
  selected.value = new Set()
  expanded.value = new Set()
  resetAddForm()
  closeOAuth()
  oauthNotice.value = null
  oauthError.value = null
  credMsg.value = {}
  // 换提供商时丢弃上一个提供商的模型探测结论
  store.clearModelTestResults()
  modelTestNote.value = null
  modelTestOk.value = false
  if (!props.provider) return
  loading.value = true
  original.value = null
  draft.value = null
  try {
    const d = await store.getDetail(props.provider.id)
    if (token !== loadToken) return
    original.value = sortedByPriority(cloneDetail(d))
    draft.value = cloneDetail(original.value)
    // 支持 OAuth 的提供商才懒加载授权能力信息（只拉一次）
    if (d.authModes.includes('oauth') && !oauthStore.loaded) void oauthStore.fetchProviders()
  } catch (e) {
    if (token !== loadToken) return
    loadError.value = e instanceof Error ? e.message : String(e)
  } finally {
    if (token === loadToken) loading.value = false
  }
}

watch(
  // 打开时加载；与已打开期间被换成另一个提供商时也要重新加载（否则会展示上一个提供商的草稿）
  [() => props.modelValue, () => props.provider?.id],
  ([open], prev) => {
    if (open) load()
    else if (prev?.[0]) {
      loadToken++
      resetAddForm()
      closeOAuth() // 卸载面板会同时取消进行中的授权会话
    }
  },
  { immediate: true },
)
onBeforeUnmount(resetAddForm)

const dirty = computed(() => !!original.value && !!draft.value && isDetailDirty(original.value, draft.value))
const issues = computed(() => (draft.value ? validateDetail(draft.value) : []))
const conflictIds = computed(() => (draft.value ? conflictModelIds(findAliasConflicts(draft.value.models)) : new Set<string>()))
const canAddKey = computed(() => !!draft.value?.adapterReady && !!draft.value.authModes.includes('apiKey'))
const noAuth = computed(() => !!draft.value?.authModes.includes('none'))
const canEnableNoAuth = computed(() => !!draft.value?.adapterReady && noAuth.value && draft.value.protocols.length > 0 && !draft.value.connections.some((c) => c.kind === 'none'))
const supportsOAuth = computed(() => !!draft.value?.authModes.includes('oauth'))
const oauthInfo = computed(() => (draft.value ? oauthStore.infoFor(draft.value.id) : undefined))
const oauthIsCustom = computed(() => oauthInfo.value?.status === 'custom')
const showDisabledApiKeyAdd = computed(() => !!draft.value && !noAuth.value && draft.value.authModes.includes('apiKey'))
const oauthButtonDisabled = computed(
  () => saving.value || oauthIsCustom.value || (oauthStore.loading && !oauthInfo.value) || oauthPanelOpen.value,
)
/** 适配器未就绪时的诚实提示：可以授权，但授权后暂不能用于转发 */
const oauthForwardNote = computed(() => {
  const d = draft.value
  if (!d || !supportsOAuth.value || oauthIsCustom.value) return null
  // 令牌兑换步骤未接入（github / muse）即使适配器就绪也不能用于转发，同样如实提示
  if (d.adapterReady && !oauthInfo.value?.tokenExchangePending) return null
  return oauthInfo.value?.limitation ?? d.pendingReason ?? '转发适配器尚未接入'
})
const pendingReason = computed(() => draft.value?.pendingReason ?? props.provider?.pendingReason ?? null)
const websiteHref = computed(() => {
  const w = draft.value?.website ?? props.provider?.website ?? null
  return w && /^https?:\/\//i.test(w) ? w : null
})
const showRisk = computed(() => !!draft.value?.riskNotice && draft.value.group !== 'custom')
const headerStatus = computed(() => providerStatus(draft.value ?? props.provider ?? { adapterReady: false, status: 'unsupported' }))
const logoColor = computed(() => draft.value?.color ?? props.provider?.color ?? null)
const logoTextIcon = computed(() => draft.value?.textIcon ?? props.provider?.textIcon ?? null)
const headerName = computed(() => draft.value?.name ?? props.provider?.name ?? '')
const connectionTotal = computed(() => draft.value?.connections.length ?? props.provider?.connectionCount ?? 0)
const allSelected = computed(
  () => !!draft.value?.connections.length && draft.value.connections.every((c) => selected.value.has(c.id)),
)

function setConnection(next: Connection) {
  if (!draft.value) return
  draft.value.connections = draft.value.connections.map((c) => (c.id === next.id ? next : c))
}
function toggleIn(which: 'selected' | 'expanded', id: string, on: boolean) {
  const setRef = which === 'selected' ? selected : expanded
  const next = new Set(setRef.value)
  if (on) next.add(id)
  else next.delete(id)
  setRef.value = next
}
function toggleAll(on: boolean) {
  selected.value = on ? new Set(draft.value?.connections.map((c) => c.id)) : new Set()
}

function resetAddForm() {
  addOpen.value = false
  addLabel.value = ''
  addKey.value = ''
  addError.value = null
}

function toggleAdd() {
  if (!canAddKey.value || saving.value) return
  addNotice.value = null
  if (addOpen.value) resetAddForm()
  else addOpen.value = true
}

function openOAuth(target: Connection | null = null) {
  if (!draft.value || saving.value) return
  oauthNotice.value = null
  oauthError.value = null
  reauthTarget.value = target
  oauthPanelKey.value++
  oauthPanelOpen.value = true
}

/** 关闭面板：v-if 卸载会让面板尽力取消仍在进行的授权会话 */
function closeOAuth() {
  oauthPanelOpen.value = false
  reauthTarget.value = null
}

/**
 * 授权成功后重新拉取详情，让连接列表以后端为准。
 * 没有未保存修改时，同时替换 original 和 draft，弹窗保持“未修改”；
 * 有未保存修改时只并入新增/变化的连接与模型，不覆盖用户正在编辑的内容。
 */
async function refreshAfterOAuth(): Promise<void> {
  const id = draft.value?.id
  if (!id) return
  await store.fetchProviders()
  const fresh = await store.getDetail(id)
  if (draft.value?.id !== id || !original.value) return
  if (!dirty.value) {
    original.value = sortedByPriority(cloneDetail(fresh))
    draft.value = cloneDetail(original.value)
  } else {
    for (const target of [original.value, draft.value]) {
      if (!target) continue
      const known = new Map(target.connections.map((c) => [c.id, c]))
      for (const c of fresh.connections) {
        const k = known.get(c.id)
        if (k) k.status = c.status
        else target.connections.push({ ...c })
      }
      const models = new Set(target.models.map((m) => m.id))
      for (const m of fresh.models) if (!models.has(m.id)) target.models.push({ ...m })
    }
  }
  syncSummary()
}

async function onOAuthDone(connectionId: string) {
  oauthError.value = null
  try {
    await refreshAfterOAuth()
    const c = draft.value?.connections.find((x) => x.id === connectionId)
    oauthNotice.value = `已通过 OAuth 授权${c ? `“${c.accountLabel}”` : ''}，令牌已保存到系统凭据库`
  } catch (e) {
    oauthError.value = `授权已完成，但刷新连接列表失败：${errText(e)}`
  }
}

function askDisconnect(c: Connection) {
  pendingDisconnect.value = c
  confirmDisconnectOpen.value = true
}

async function doDisconnect() {
  const c = pendingDisconnect.value
  if (!c || oauthBusyIds.value.has(c.id)) return
  pendingDisconnect.value = null
  oauthNotice.value = null
  oauthError.value = null
  oauthBusyIds.value = new Set(oauthBusyIds.value).add(c.id)
  try {
    await oauthStore.disconnect(c.id)
    for (const target of [original.value, draft.value]) {
      if (target) target.connections = target.connections.filter((x) => x.id !== c.id)
    }
    toggleIn('selected', c.id, false)
    toggleIn('expanded', c.id, false)
    if (reauthTarget.value?.id === c.id) closeOAuth()
    await store.fetchProviders()
    syncSummary()
    oauthNotice.value = `已断开连接“${c.accountLabel}”，令牌条目已删除`
  } catch (e) {
    oauthError.value = `断开失败：${oauthErrorText(toOAuthApiError(e).code)}`
  } finally {
    const next = new Set(oauthBusyIds.value)
    next.delete(c.id)
    oauthBusyIds.value = next
  }
}

function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

/** 拉取模型：把后端合并后的最新列表并入草稿，保留用户已改的别名与启用状态 */
const modelsBusy = ref(false)
const modelsError = ref<string | null>(null)
/** 模型级探测：复用 store 的结果表（key = modelId），并给出就地提示 */
async function testModel(modelId: string) {
  const d = draft.value
  if (!d) return
  modelTestNote.value = null
  const model = d.models.find((m) => m.id === modelId)
  if (!original.value?.models.some((m) => m.id === modelId)) {
    modelTestOk.value = false
    modelTestNote.value = '请先保存修改，再测试新添加的模型'
    return
  }
  const r = await store.testModel(d.id, modelId)
  const label = model?.publicAlias || model?.upstreamModelId || modelId
  modelTestOk.value = r.ok
  modelTestNote.value = r.ok
    ? `「${label}」可用${r.latencyMs != null ? `（${r.latencyMs}ms）` : ''}`
    : `「${label}」探测失败：${r.error ?? '未知错误'}`
}
async function onFetchModels() {
  const d = draft.value
  if (!d || modelsBusy.value) return
  modelsError.value = null
  modelsBusy.value = true
  // 模型列表变化后旧的探测结论不再适用
  store.clearModelTestResults()
  modelTestNote.value = null
  modelTestOk.value = false
  try {
    const fresh = await store.fetchModels(d.id)
    const byId = new Map(d.models.map((m) => [m.id, m]))
    const byUpstream = new Map(d.models.map((m) => [m.upstreamModelId, m]))
    d.models = fresh
      .map((m) => {
        const prev = byId.get(m.id) ?? byUpstream.get(m.upstreamModelId)
        return prev ?? m
      })
      .concat(
        // 保留用户手动添加、但上游本次未返回的模型（例如自定义别名）
        d.models.filter((m) => !fresh.some((f) => f.id === m.id || f.upstreamModelId === m.upstreamModelId)),
      )
    if (!fresh.length) modelsError.value = '上游与内置模型库都没有返回模型'
  } catch (e) {
    modelsError.value = `拉取模型失败：${errText(e)}`
  } finally {
    modelsBusy.value = false
  }
}

/** 把后端刚刷新的摘要（状态、连接数）同步进弹窗里的快照，保持“未修改”状态 */
function syncSummary() {
  const id = draft.value?.id
  const fresh = store.providers.find((p) => p.id === id)
  if (!fresh) return
  for (const d of [original.value, draft.value]) {
    if (d) {
      d.status = fresh.status
      d.connectionCount = fresh.connectionCount
    }
  }
}

async function submitAdd() {
  const d = draft.value
  if (!d || adding.value || !canAddKey.value) return
  const labelIssue = validateAccountLabel(addLabel.value)
  const keyIssue = validateApiKey(addKey.value)
  if (labelIssue || keyIssue) {
    addError.value = labelIssue ?? keyIssue
    return
  }
  // 取出密钥后立即清空输入框；此后 apiKey 只存在于下面这次调用的参数里
  const apiKey = addKey.value
  addKey.value = ''
  addError.value = null
  adding.value = true
  try {
    const conn = await store.addConnection({ providerId: d.id, accountLabel: addLabel.value.trim(), apiKey })
    for (const target of [original.value, draft.value]) target?.connections.push({ ...conn })
    syncSummary()
    addNotice.value = `已保存“${conn.accountLabel}”的 API Key`
    resetAddForm()
  } catch (e) {
    addError.value = `添加失败：${errText(e)}（API Key 输入框已清空，请重新输入）`
  } finally {
    adding.value = false
  }
}

async function enableNoAuth() {
  const d = draft.value
  if (!d || !canEnableNoAuth.value || adding.value) return
  addError.value = null
  addNotice.value = null
  adding.value = true
  try {
    const conn = await store.addConnection({ providerId: d.id, accountLabel: '免密钥连接', apiKey: '' })
    for (const target of [original.value, draft.value]) {
      if (target && !target.connections.some((c) => c.id === conn.id)) target.connections.push({ ...conn })
    }
    syncSummary()
    addNotice.value = '免密钥连接已启用，可直接使用可用模型'
  } catch (e) {
    addError.value = `启用失败：${errText(e)}`
  } finally {
    adding.value = false
  }
}

async function updateCredential(c: Connection, apiKey: string) {
  credBusy.value = new Set(credBusy.value).add(c.id)
  const { [c.id]: _drop, ...rest } = credMsg.value
  credMsg.value = rest
  try {
    await store.updateConnectionCredential(c.id, apiKey)
    credMsg.value = { ...credMsg.value, [c.id]: { ok: true, text: 'API Key 已更新' } }
  } catch (e) {
    credMsg.value = { ...credMsg.value, [c.id]: { ok: false, text: `更新失败：${errText(e)}` } }
  } finally {
    const next = new Set(credBusy.value)
    next.delete(c.id)
    credBusy.value = next
  }
}

async function removeProvider() {
  const d = draft.value
  if (!d || d.group !== 'custom' || deletingProvider.value) return
  deletingProvider.value = true
  saveError.value = null
  try {
    await store.deleteCustomProvider(d.id)
    emit('update:modelValue', false)
  } catch (e) {
    saveError.value = `删除失败：${errText(e)}`
  } finally {
    deletingProvider.value = false
  }
}

function askRemove(c: Connection) {
  pendingDelete.value = c
  confirmDeleteOpen.value = true
}
function confirmRemove() {
  const c = pendingDelete.value
  if (!c || !draft.value) return
  draft.value.connections = draft.value.connections.filter((x) => x.id !== c.id)
  toggleIn('selected', c.id, false)
  toggleIn('expanded', c.id, false)
  pendingDelete.value = null
}

async function testOne(c: Connection) {
  await store.testConnection(c.id)
}
/** 逐个测试：优先测选中项，没有选中则测全部已启用且已保存的连接 */
async function testBatch() {
  const d = draft.value
  if (!d || batchTesting.value) return
  const pool = d.connections
  const picked = pool.filter((c) => selected.value.has(c.id))
  const targets = picked.length ? picked : pool.filter((c) => c.enabled)
  if (!targets.length) return
  batchTesting.value = true
  try {
    const limit = concurrentTest.value ? Math.max(1, Math.min(10, parallel.value || 1)) : 1
    const queue = [...targets]
    await Promise.all(
      Array.from({ length: Math.min(limit, queue.length) }, async () => {
        while (queue.length) await testOne(queue.shift() as Connection)
      }),
    )
  } finally {
    batchTesting.value = false
  }
}

function toInt(v: string, fallback: number): number {
  const n = Number(v)
  return Number.isFinite(n) ? Math.trunc(n) : fallback
}

async function save() {
  const d = draft.value
  if (!d || saving.value || !dirty.value) return
  showIssues.value = true
  // 校验不通过（含别名冲突）时阻止保存并提示，绝不静默覆盖
  if (validateDetail(d).length) return
  saving.value = true
  saveError.value = null
  try {
    const saved = await store.saveProvider(toSavePayload(d))
    original.value = sortedByPriority(cloneDetail(saved))
    draft.value = cloneDetail(original.value)
    emit('saved', saved)
    emit('update:modelValue', false)
  } catch (e) {
    saveError.value = e instanceof Error ? e.message : String(e)
  } finally {
    saving.value = false
  }
}

function cancel() {
  modalRef.value?.requestClose()
}
</script>

<template>
  <BaseModal
    ref="modalRef"
    :model-value="modelValue"
    :title="headerName || '提供商'"
    :dirty="dirty"
    width="860px"
    @update:model-value="emit('update:modelValue', $event)"
  >
    <template #header>
      <div class="head">
        <ProviderLogo :name="headerName || '?'" :size="48" :color="logoColor" :text-icon="logoTextIcon" />
        <div class="head-text">
          <input
            v-if="draft?.group === 'custom'"
            v-model="draft.name"
            class="input name-input"
            placeholder="提供商名称"
            aria-label="提供商名称"
            :disabled="saving"
          />
          <h2 v-else>{{ headerName }}</h2>
          <div class="head-meta">
            <StatusTag :tone="headerStatus.tone">{{ headerStatus.text }}</StatusTag>
            <span>{{ connectionTotal }} 个账号或密钥</span>
            <span class="sep" aria-hidden="true">|</span>
            <span>{{ draft?.adapterReady === false || !draft ? (draft ? '适配器待接入' : '') : protocolText(draft.protocols) }}</span>
          </div>
        </div>
      </div>
    </template>

    <div v-if="loading" class="state" role="status">加载中…</div>
    <div v-else-if="loadError" class="state bad" role="alert">
      <p>加载失败：{{ loadError }}</p>
      <button type="button" class="btn" @click="load">重试</button>
    </div>

    <div v-else-if="draft" class="content">
      <div v-if="!draft.adapterReady" class="notice neutral pending" role="note">
        <span>
          该提供商的适配器尚未接入，暂不可配置，也不会参与路由。
          <template v-if="pendingReason"><br /><strong>待接入原因：</strong>{{ pendingReason }}</template>
        </span>
      </div>
      <div v-if="showRisk" class="notice risk" role="note">
        <AlertTriangle :size="16" aria-hidden="true" />
        <span><strong>风险提示：</strong>{{ draft.riskNotice }}</span>
      </div>

      <!-- 连接 -->
      <section class="panel" aria-label="连接">
        <div class="panel-head">
          <div class="panel-title">
            <h3>账号与密钥</h3>
            <p v-if="noAuth">{{ draft.protocols.length ? '无需账号或 API Key，点击启用即可使用' : '无需 API Key，可直接使用对应的媒体功能' }}</p>
            <p v-else-if="supportsOAuth && canAddKey">可以填写 {{ draft.name }} 的 API Key，或使用登录验证；支持多个账号</p>
            <p v-else-if="supportsOAuth">登录 {{ draft.name }} 账号后即可使用，支持多个账号</p>
            <p v-else>填写从 {{ draft.name }} 官网获取的 API Key，支持添加多个密钥</p>
          </div>
          <div class="test-tools">
            <button type="button" class="btn" :disabled="readonly || batchTesting || !draft.connections.length" @click="testBatch">
              <Zap :size="14" /> {{ batchTesting ? '测试中…' : '测试连接 (逐个)' }}
            </button>
            <label class="inline">
              并发测试
              <ToggleSwitch v-model="concurrentTest" label="并发测试" :disabled="readonly" />
            </label>
            <label class="inline">
              并行数
              <input
                class="input num"
                type="number"
                min="1"
                max="10"
                :value="parallel"
                :disabled="readonly || !concurrentTest"
                aria-label="并行数"
                @input="parallel = Math.max(1, Math.min(10, toInt(($event.target as HTMLInputElement).value, 1)))"
              />
            </label>
          </div>
        </div>

        <label v-if="draft.connections.length" class="select-all">
          <input
            type="checkbox"
            :checked="allSelected"
            aria-label="全选连接"
            @change="toggleAll(($event.target as HTMLInputElement).checked)"
          />
          全选
        </label>
        <p v-else class="empty">{{ noAuth ? '尚未启用免密钥连接。' : '还没有添加账号或 API Key。' }}</p>

        <ul class="conns">
          <ConnectionRow
            v-for="(c, i) in draft.connections"
            :key="c.id"
            :connection="c"
            :index="i"
            :selected="selected.has(c.id)"
            :expanded="expanded.has(c.id)"
            :result="store.testResults[c.id]"
            :testing="store.testingIds.has(c.id)"
            :credential-busy="credBusy.has(c.id)"
            :credential-message="credMsg[c.id] ?? null"
            :oauth-busy="oauthBusyIds.has(c.id)"
            :readonly="readonly"
            @update:connection="setConnection"
            @update:selected="toggleIn('selected', c.id, $event)"
            @update:expanded="toggleIn('expanded', c.id, $event)"
            @remove="askRemove(c)"
            @test="testOne(c)"
            @update-credential="updateCredential(c, $event)"
            @reauthorize="openOAuth(c)"
            @disconnect="askDisconnect(c)"
          />
        </ul>

        <p v-if="addNotice" class="notice-ok" role="status">{{ addNotice }}</p>
        <p v-if="noAuth && addError" class="issues" role="alert">{{ addError }}</p>
        <p v-if="oauthNotice" class="notice-ok" role="status">{{ oauthNotice }}</p>
        <p v-if="oauthError" class="issues oauth-err" role="alert">{{ oauthError }}</p>

        <OAuthPanel
          v-if="oauthPanelOpen"
          :key="oauthPanelKey"
          :provider-id="draft.id"
          :provider-name="draft.name"
          :reauth-connection="reauthTarget"
          :hide-risk-notice="showRisk"
          :auto-start="draft.group === 'oauth' && !reauthTarget"
          @done="onOAuthDone"
          @close="closeOAuth"
        />

        <form v-if="addOpen" class="add-form" aria-label="添加 API Key" @submit.prevent="submitAdd">
          <label class="field">
            <span>备注名称</span>
            <input
              v-model="addLabel"
              class="input"
              maxlength="128"
              autocomplete="off"
              placeholder="例如：个人 / 团队"
              aria-label="名称"
              :disabled="adding"
            />
          </label>
          <label class="field">
            <span>API Key</span>
            <input
              v-model="addKey"
              class="input"
              type="password"
              autocomplete="new-password"
              spellcheck="false"
              placeholder="粘贴 API Key"
              aria-label="API Key"
              :disabled="adding"
            />
          </label>
          <p class="hint">
            密钥只保存到系统凭据库，界面、数据库与日志都不会保留明文。
            <a v-if="websiteHref" :href="websiteHref" target="_blank" rel="noopener noreferrer" class="link">
              前往官网获取 Key <ExternalLink :size="12" aria-hidden="true" />
            </a>
          </p>
          <p v-if="addError" class="issues" role="alert">{{ addError }}</p>
          <div class="add-actions">
            <button type="button" class="btn" :disabled="adding" @click="resetAddForm">取消</button>
            <button type="submit" class="btn btn-primary" :disabled="adding">{{ adding ? '保存中…' : '保存 API Key' }}</button>
          </div>
        </form>

        <div class="add-row">
          <button v-if="canEnableNoAuth" type="button" class="btn btn-primary add" :disabled="saving || adding" @click="enableNoAuth">
            <Zap :size="14" /> {{ adding ? '启用中…' : '一键启用' }}
          </button>
          <button v-if="canAddKey" type="button" class="btn btn-primary add" :disabled="saving || adding" @click="toggleAdd">
            <Plus :size="14" /> 添加 API Key
          </button>
          <button
            v-else-if="showDisabledApiKeyAdd"
            type="button"
            class="btn btn-primary add"
            disabled
            :title="pendingReason ?? undefined"
          >
            <Plus :size="14" /> 添加 API Key
          </button>
          <button
            v-if="supportsOAuth && !oauthIsCustom"
            type="button"
            class="btn"
            :disabled="oauthButtonDisabled"
            @click="openOAuth()"
          >
            <Lock :size="14" /> 登录验证
          </button>
        </div>
        <div v-if="supportsOAuth && oauthIsCustom && oauthInfo" class="oauth-custom" role="note" data-testid="oauth-custom-notice">
          <p><strong>该提供商使用专有登录方式，本版本暂不支持 OAuth 授权。</strong></p>
          <p v-if="oauthInfo.reason" data-testid="oauth-custom-reason"><strong>原因：</strong>{{ oauthInfo.reason }}</p>
          <template v-if="oauthInfo.requires.length">
            <p><strong>接入需要：</strong></p>
            <ul>
              <li v-for="r in oauthInfo.requires" :key="r">{{ r }}</li>
            </ul>
          </template>
        </div>
        <p v-else-if="oauthForwardNote" class="oauth-forward" role="note" data-testid="oauth-forward-note">
          授权后暂不能用于转发：{{ oauthForwardNote }}
        </p>
      </section>

      <ModelList
        :key="draft.id"
        :provider-id="draft.id"
        v-model:models="draft.models"
        v-model:thinking-level="draft.thinkingLevel"
        :conflict-ids="conflictIds"
        :readonly="readonly"
        :fetching="modelsBusy"
        :test-results="store.modelTestResults"
        :testing-ids="store.testingModelIds"
        @fetch="onFetchModels"
        @test="testModel"
      />
      <p v-if="modelsError" class="issues" role="alert" data-testid="models-error">{{ modelsError }}</p>
      <p
        v-if="modelTestNote"
        class="model-test-note"
        :class="{ bad: !modelTestOk }"
        role="status"
        data-testid="model-test-note"
      >
        {{ modelTestNote }}
      </p>

      <!-- 高级配置放在模型之后 -->
      <section class="panel" aria-label="分发策略">
        <div class="panel-title"><h3>分发策略</h3><p>控制多个连接之间的请求分发与失败重试</p></div>
        <div class="policy">
          <label class="inline">
            轮询
            <ToggleSwitch v-model="draft.distribution.roundRobin" label="轮询" :disabled="readonly" />
          </label>
          <label class="inline">
            最大并发
            <input
              class="input num"
              type="number"
              min="1"
              step="1"
              :value="draft.distribution.maxConcurrency"
              :disabled="readonly"
              aria-label="最大并发"
              @input="draft.distribution.maxConcurrency = toInt(($event.target as HTMLInputElement).value, 0)"
            />
          </label>
          <label class="inline">
            重试次数
            <input
              class="input num"
              type="number"
              min="0"
              step="1"
              :value="draft.distribution.maxRetries"
              :disabled="readonly"
              aria-label="重试次数"
              @input="draft.distribution.maxRetries = toInt(($event.target as HTMLInputElement).value, -1)"
            />
          </label>
        </div>
      </section>

      <HeadersEditor v-model="draft.customHeaders" :readonly="readonly" />
    </div>

    <template #footer>
      <div class="foot">
        <button
          v-if="draft?.group === 'custom'"
          type="button"
          class="btn btn-danger"
          :disabled="deletingProvider || saving"
          aria-label="删除提供商"
          @click="confirmDeleteProviderOpen = true"
        >
          <Trash2 :size="14" /> {{ deletingProvider ? '删除中…' : '删除提供商' }}
        </button>
        <div class="foot-msg">
          <div v-if="showIssues && issues.length" class="issues" role="alert">
            <p v-for="(it, i) in issues" :key="i">{{ it.message }}</p>
          </div>
          <p v-else-if="saveError" class="issues" role="alert">保存失败：{{ saveError }}</p>
          <span v-else-if="dirty" class="dirty-tip">有未保存的修改</span>
        </div>
        <button type="button" class="btn" @click="cancel">取消</button>
        <button
          type="button"
          class="btn"
          :class="{ 'btn-primary': dirty }"
          :disabled="!dirty || saving || readonly"
          :aria-label="dirty ? '保存修改（有未保存的更改）' : '保存修改'"
          @click="save"
        >
          {{ saving ? '保存中…' : '保存修改' }}
        </button>
      </div>
    </template>
  </BaseModal>

  <ConfirmDialog
    v-model="confirmDeleteOpen"
    title="删除连接"
    :message="`确定删除连接“${pendingDelete?.accountLabel || '未命名'}”吗？点击“保存修改”后才会真正生效，取消则不会删除。`"
    confirm-text="删除"
    danger
    @confirm="confirmRemove"
  />

  <ConfirmDialog
    v-model="confirmDisconnectOpen"
    title="断开 OAuth 连接"
    :message="`确定断开连接“${pendingDisconnect?.accountLabel || '未命名'}”吗？系统凭据库中的令牌条目会被删除，该连接会立即移除（不需要再点“保存修改”），且无法恢复；之后如需使用，必须重新授权。`"
    confirm-text="断开"
    danger
    @confirm="doDisconnect"
  />

  <ConfirmDialog
    v-model="confirmDeleteProviderOpen"
    title="删除自定义提供商"
    :message="`确定删除自定义提供商“${draft?.name || headerName}”吗？它的全部连接和已保存的 API Key 都会被清除，且无法恢复。`"
    confirm-text="删除"
    danger
    @confirm="removeProvider"
  />
</template>

<style scoped>
.head { display: flex; align-items: center; gap: 14px; flex: 1; min-width: 0; }
.head-text { min-width: 0; }
.head h2 { margin: 0 0 6px; font-size: 20px; font-weight: 600; }
.name-input { width: 280px; margin-bottom: 6px; font-size: 16px; font-weight: 600; }
.head-meta { display: flex; align-items: center; gap: 10px; font-size: 13px; color: var(--c-text-2); }
.sep { color: var(--c-border); }

.state { display: flex; flex-direction: column; align-items: center; gap: 10px; padding: 48px 0; color: var(--c-text-2); }
.state.bad { color: var(--c-danger); }
.state p { margin: 0; }

.content { display: flex; flex-direction: column; gap: 14px; }
.notice { display: flex; align-items: center; gap: 8px; padding: 10px 14px; border-radius: var(--radius); font-size: 13px; }
.notice.risk { background: var(--c-warning-soft); border: 1px solid var(--c-warning-border); color: var(--c-warning); }
.notice.neutral { background: var(--c-surface-2); color: var(--c-text-2); }

.panel { border: 1px solid var(--c-border); border-radius: var(--radius); padding: 14px 16px 16px; background: var(--c-canvas); }
.panel-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; flex-wrap: wrap; margin-bottom: 10px; }
.panel-title h3 { margin: 0; font-size: 15px; font-weight: 600; }
.panel-title p { margin: 2px 0 0; font-size: 12px; color: var(--c-text-2); }
.test-tools { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
.inline { display: inline-flex; align-items: center; gap: 8px; font-size: 13px; color: var(--c-text-2); }
.num { width: 72px; }
.select-all { display: inline-flex; align-items: center; gap: 8px; margin-bottom: 8px; font-size: 13px; color: var(--c-text-2); }
.select-all input { width: 16px; height: 16px; accent-color: var(--c-brand); }
.empty { margin: 4px 0 10px; font-size: 13px; color: var(--c-text-2); }
.conns { display: flex; flex-direction: column; gap: 8px; margin: 0 0 12px; padding: 0; list-style: none; }
.policy { display: flex; flex-wrap: wrap; gap: 24px; margin-top: 12px; }

.notice.pending { align-items: flex-start; }
.notice-ok { margin: 0 0 10px; font-size: 12px; color: var(--c-success); }
.add-form { display: flex; flex-direction: column; gap: 10px; margin: 0 0 12px; padding: 12px 14px; border: 1px dashed var(--c-border); border-radius: var(--radius); background: var(--c-sidebar); }
.field { display: flex; flex-direction: column; gap: 4px; font-size: 12px; color: var(--c-text-2); }
.hint { margin: 0; font-size: 12px; color: var(--c-text-2); }
.link { display: inline-flex; align-items: center; gap: 3px; margin-left: 6px; color: var(--c-brand); text-decoration: none; }
.link:hover { text-decoration: underline; }
.add-actions { display: flex; justify-content: flex-end; gap: 8px; }
.add-row { display: flex; gap: 8px; flex-wrap: wrap; }
.oauth-custom { margin-top: 10px; padding: 10px 14px; border-radius: var(--radius); background: var(--c-surface-2); color: var(--c-text-2); font-size: 13px; }
.oauth-custom p { margin: 0 0 4px; }
.oauth-custom ul { margin: 0; padding-left: 18px; }
.oauth-forward { margin: 8px 0 0; font-size: 12px; color: var(--c-text-2); }
.oauth-err { margin-bottom: 10px; }
.foot { display: flex; align-items: center; gap: 10px; width: 100%; }
.foot-msg { flex: 1; min-width: 0; }
.issues { margin: 0; font-size: 12px; color: var(--c-danger); }
.issues p { margin: 0; }
.model-test-note { margin: 0; padding: 8px 12px; border-radius: 8px; background: var(--c-surface-2); font-size: var(--fs-small); color: var(--c-text); }
.model-test-note.bad { background: var(--c-danger-soft); color: var(--c-danger); }
.dirty-tip { font-size: 12px; color: var(--c-brand); }
</style>
