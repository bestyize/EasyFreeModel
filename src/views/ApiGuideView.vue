<script setup lang="ts">
import { computed, onActivated, onMounted, ref, watch, type Component } from 'vue'
import { ArrowLeftRight, Sparkles, Terminal, TriangleAlert } from 'lucide-vue-next'
import { api } from '@/api'
import ClientImportModal from '@/components/guide/ClientImportModal.vue'
import CcSwitchModal from '@/components/guide/CcSwitchModal.vue'
import CodeBlock from '@/components/guide/CodeBlock.vue'
import CopyButton from '@/components/guide/CopyButton.vue'
import SegTabs from '@/components/guide/SegTabs.vue'
import {
  EXAMPLE_LANGS,
  EXAMPLE_PROTOCOLS,
  KEY_PLACEHOLDER,
  MODEL_PLACEHOLDER,
  endpointRows,
  gatewayOrigin,
  isWildcardAddress,
  langsFor,
  requestExample,
  type ExampleLang,
  type ExampleProtocol,
} from '@/components/guide/snippets'
import { PROTOCOL_OPTIONS } from '@/components/settings/protocols'
import { errorMessage } from '@/components/media/format'
import { useApiKeysStore } from '@/stores/apiKeys'
import { useSettingsStore } from '@/stores/settings'
import { useSystemStore } from '@/stores/system'
import type { ApiKeyInfo, GatewayModel } from '@/types'

const keysStore = useApiKeysStore()
const settings = useSettingsStore()
const system = useSystemStore()

// ───────── 网关地址 ─────────

const running = computed(() => system.status?.gateway.running ?? false)
/** 运行中以实际监听为准，否则用设置里保存的值 */
const listen = computed(() => {
  const g = system.status?.gateway
  if (g?.running) return { address: g.address, port: g.port }
  const s = settings.saved
  return s ? { address: s.listenAddress, port: s.port } : null
})
const origin = computed(() => (listen.value ? gatewayOrigin(listen.value.address, listen.value.port) : ''))
const wildcard = computed(() => isWildcardAddress(listen.value?.address))
const rows = computed(() => endpointRows(origin.value))
const starting = ref(false)
const startError = ref<string | null>(null)

async function startGateway() {
  starting.value = true
  startError.value = null
  try {
    await system.toggleGateway()
    if (system.status?.gateway.error) startError.value = system.status.gateway.error
  } catch (e) {
    startError.value = errorMessage(e)
  } finally {
    starting.value = false
  }
}

// ───────── 密钥与模型 ─────────

const ALL_PROTOCOLS = PROTOCOL_OPTIONS.filter((p) => p.implemented).map((p) => p.id)
const keyId = ref('')
const modelText = ref('')
const models = ref<GatewayModel[]>([])
const modelsError = ref<string | null>(null)
const creating = ref(false)
const createError = ref<string | null>(null)

function isLive(k: ApiKeyInfo) {
  return !k.disabled && !(k.expiresAt && new Date(k.expiresAt).getTime() <= Date.now())
}
/** 能直接拿来接入的密钥：未停用、未过期、本机保存了明文 */
const usableKeys = computed(() => keysStore.keys.filter((k) => isLive(k) && k.secret))
const legacyOnly = computed(() => usableKeys.value.length === 0 && keysStore.keys.some((k) => isLive(k) && !k.secret))
const fullKey = computed(() => usableKeys.value.find((k) => k.id === keyId.value)?.secret ?? '')

watch(
  usableKeys,
  (list) => {
    if (list.some((k) => k.id === keyId.value)) return
    const best = list.find((k) => ALL_PROTOCOLS.every((p) => k.protocols.includes(p))) ?? list[0]
    keyId.value = best?.id ?? ''
  },
  { immediate: true },
)
watch(models, (list) => {
  if (!modelText.value && list.length) modelText.value = list[0].id
})

async function createKey() {
  creating.value = true
  createError.value = null
  try {
    await keysStore.create({
      name: 'Router 接入',
      expiresAt: null,
      protocols: ALL_PROTOCOLS,
      allowedModels: [],
      rpmLimit: null,
      tpmLimit: null,
      totalLimit: null,
    })
  } catch (e) {
    createError.value = `创建密钥失败：${errorMessage(e)}`
  } finally {
    creating.value = false
  }
}

// ───────── 一键接入 ─────────

type Client = 'codex' | 'claude' | 'ccswitch'
const CLIENTS: { id: Client; label: string; desc: string; icon: Component }[] = [
  { id: 'codex', label: 'Codex', desc: '写入 ~/.codex/config.toml', icon: Terminal },
  { id: 'claude', label: 'Claude Code', desc: '写入 ~/.claude/settings.json', icon: Sparkles },
  { id: 'ccswitch', label: 'CC Switch', desc: '交给 CC Switch 管理', icon: ArrowLeftRight },
]
const canImport = computed(() => !!origin.value && !!fullKey.value && !!modelText.value.trim())
const importInput = computed(() => ({ origin: origin.value, key: fullKey.value, model: modelText.value.trim() }))

const dialog = ref<Client | null>(null)
function dialogModel(c: Client) {
  return computed({
    get: () => dialog.value === c,
    set: (open: boolean) => {
      if (!open && dialog.value === c) dialog.value = null
    },
  })
}
const codexOpen = dialogModel('codex')
const claudeOpen = dialogModel('claude')
const ccSwitchOpen = dialogModel('ccswitch')

// ───────── 请求示例 ─────────

const protocol = ref<ExampleProtocol>('openai-chat')
const lang = ref<ExampleLang>('curl')
const langOptions = computed(() => EXAMPLE_LANGS.filter((l) => langsFor(protocol.value).includes(l.id)))
watch(protocol, (p) => {
  if (!langsFor(p).includes(lang.value)) lang.value = 'curl'
})
const exampleText = computed(
  () =>
    requestExample(protocol.value, lang.value, {
      origin: origin.value,
      key: fullKey.value || KEY_PLACEHOLDER,
      model: modelText.value.trim() || MODEL_PLACEHOLDER,
    }) ?? '',
)

// ───────── 加载 ─────────

async function loadModels() {
  try {
    models.value = await api.listGatewayModels()
    modelsError.value = null
  } catch (e) {
    modelsError.value = errorMessage(e)
  }
}
function refresh() {
  void keysStore.load()
  void settings.ensureLoaded()
  void loadModels()
}
let activatedOnce = false
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
    <div v-if="!running && listen" class="banner warn" role="status" data-testid="stopped-banner">
      <TriangleAlert :size="18" aria-hidden="true" />
      <span>本地网关当前未运行，客户端暂时连不上。</span>
      <button class="btn" type="button" data-testid="start-gateway" :disabled="starting" @click="startGateway">{{ starting ? '启动中…' : '启动网关' }}</button>
    </div>
    <p v-if="startError" class="banner danger" role="alert" data-testid="start-error">{{ startError }}</p>

    <section class="card section" aria-labelledby="g-params">
      <h2 id="g-params" class="section-title">连接参数</h2>

      <div v-if="keysStore.loaded && usableKeys.length === 0" class="empty" data-testid="no-key">
        <p>{{ legacyOnly ? '已有的密钥没有保存明文，没法直接用于接入。' : '还没有可用的 API 密钥。' }}</p>
        <button class="btn btn-primary" type="button" data-testid="create-key" :disabled="creating" @click="createKey">
          {{ creating ? '创建中…' : '一键创建密钥' }}
        </button>
      </div>
      <p v-if="createError" class="warn-text" role="alert" data-testid="create-error">{{ createError }}</p>

      <div class="params">
        <div v-if="usableKeys.length" class="field">
          <label for="g-key">API 密钥</label>
          <select id="g-key" v-model="keyId" class="input" data-testid="key-select">
            <option v-for="k in usableKeys" :key="k.id" :value="k.id">{{ k.name }}</option>
          </select>
          <div class="keyline">
            <code data-testid="key-full">{{ fullKey }}</code>
            <CopyButton :text="fullKey" label="复制密钥" />
          </div>
        </div>

        <div class="field">
          <label for="g-model">模型</label>
          <input
            id="g-model" v-model="modelText" class="input" list="g-model-list" spellcheck="false" autocomplete="off"
            data-testid="model-input" placeholder="例如 openai/gpt-4o-mini"
          />
          <datalist id="g-model-list"><option v-for="m in models" :key="m.id" :value="m.id">{{ m.providerName }}</option></datalist>
          <p v-if="modelsError" class="field-note warn-text" role="alert">读取模型列表失败：{{ modelsError }}</p>
          <p v-else-if="models.length === 0" class="field-note" data-testid="no-models">
            网关还没有可用模型：请先在“提供商”页添加并启用模型，也可以手动输入模型名。
          </p>
          <p v-else class="field-note">默认选第一个，可从列表选择或手动输入。</p>
        </div>
      </div>
    </section>

    <section class="card section" aria-labelledby="g-import">
      <h2 id="g-import" class="section-title">一键接入</h2>
      <p class="lead">选一个客户端，确认后自动写入它的配置文件（会先备份），不用手动复制粘贴。</p>
      <div class="clients">
        <button
          v-for="c in CLIENTS" :key="c.id" class="client" type="button" :disabled="!canImport"
          :data-testid="`open-${c.id}`" @click="dialog = c.id"
        >
          <component :is="c.icon" :size="22" aria-hidden="true" />
          <span class="c-text"><strong>{{ c.label }}</strong><small>{{ c.desc }}</small></span>
        </button>
      </div>
      <p v-if="!canImport" class="field-note" data-testid="import-disabled-hint">先有一个可用的密钥并填好模型，这里才能点。</p>
    </section>

    <section class="card section" aria-labelledby="g-example">
      <h2 id="g-example" class="section-title">请求示例</h2>
      <div class="tabrow">
        <SegTabs v-model="protocol" :options="EXAMPLE_PROTOCOLS" label="协议" />
        <SegTabs v-model="lang" :options="langOptions" label="语言" />
      </div>
      <CodeBlock :text="exampleText" :title="lang === 'curl' ? 'shell' : lang" data-testid="example-code" />
    </section>

    <section class="card section" aria-labelledby="g-addr">
      <h2 id="g-addr" class="section-title">网关地址</h2>
      <p v-if="wildcard" class="hint" data-testid="wildcard-hint">
        网关监听在 {{ listen?.address }}，本机客户端用 127.0.0.1 即可；局域网内的其它设备请把地址里的 127.0.0.1 换成本机的局域网 IP。
      </p>
      <div v-if="origin" class="table-wrap">
        <table>
          <thead>
            <tr><th>协议</th><th>请求地址</th><th>Base URL</th></tr>
          </thead>
          <tbody>
            <tr v-for="r in rows" :key="r.id" :data-testid="`addr-${r.id}`">
              <td class="proto">{{ r.label }}</td>
              <td><span class="cell"><code>{{ r.url }}</code><CopyButton :text="r.url" :label="`复制 ${r.label} 请求地址`" /></span></td>
              <td>
                <span v-if="r.baseUrl" class="cell"><code>{{ r.baseUrl }}</code><CopyButton :text="r.baseUrl" :label="`复制 ${r.label} Base URL`" /></span>
                <span v-else class="muted">—</span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <p v-else class="muted">正在读取网关设置…</p>
    </section>

    <ClientImportModal v-model="codexOpen" client="codex" :input="importInput" />
    <ClientImportModal v-model="claudeOpen" client="claude" :input="importInput" />
    <CcSwitchModal v-model="ccSwitchOpen" :input="importInput" />
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 16px; }
.section { padding: 20px 24px; display: flex; flex-direction: column; gap: 12px; }
.lead { margin: 0; color: var(--c-text-2); font-size: var(--fs-small); line-height: 1.7; }
.hint { margin: 0; padding: 8px 12px; border-radius: 8px; background: #fdf6e8; color: #7a5a14; font-size: var(--fs-small); line-height: 1.6; }
.muted { color: var(--c-text-2); }
.banner { display: flex; align-items: center; gap: 10px; margin: 0; padding: 10px 14px; border-radius: var(--radius); font-size: var(--fs-small); line-height: 1.6; }
.banner svg { flex: none; }
.banner > span { flex: 1; }
.banner.warn { border: 1px solid #f1deb0; background: #fdf6e8; color: #7a5a14; }
.banner.danger { border: 1px solid #f3c9c9; background: #fdecec; color: #a63232; }
.empty { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 12px 14px; border: 1px dashed var(--c-border); border-radius: 8px; }
.empty p { margin: 0; font-size: var(--fs-small); }
.params { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 16px; align-items: start; }
.field { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
.field label { font-size: var(--fs-small); font-weight: 500; }
.keyline { display: flex; align-items: center; gap: 8px; min-width: 0; }
.keyline code { flex: 1; min-width: 0; padding: 6px 10px; border-radius: 6px; background: var(--c-surface-2); font-family: var(--font-mono); font-size: var(--fs-caption); word-break: break-all; user-select: all; }
.field-note { margin: 0; color: var(--c-text-2); font-size: var(--fs-caption); line-height: 1.6; }
.warn-text { margin: 0; color: #a0620a; font-size: var(--fs-small); line-height: 1.6; }
.field-note.warn-text { font-size: var(--fs-caption); }
.clients { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 12px; }
.client { display: flex; align-items: center; gap: 12px; padding: 14px 16px; border: 1px solid var(--c-border); border-radius: var(--radius); background: var(--c-canvas); color: var(--c-text); text-align: left; }
.client:hover:not(:disabled) { border-color: var(--c-brand); background: var(--c-brand-soft); }
.client:disabled { opacity: 0.5; cursor: not-allowed; }
.client svg { flex: none; color: var(--c-brand); }
.c-text { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.c-text strong { font-size: var(--fs-body); }
.c-text small { color: var(--c-text-2); font-size: var(--fs-caption); font-family: var(--font-mono); }
.tabrow { display: flex; flex-wrap: wrap; gap: 12px; }
.table-wrap { overflow-x: auto; border: 1px solid var(--c-border); border-radius: 8px; }
table { width: 100%; border-collapse: collapse; font-size: var(--fs-small); }
th { padding: 9px 14px; text-align: left; color: var(--c-text-2); font-weight: 500; background: var(--c-sidebar); border-bottom: 1px solid var(--c-border); white-space: nowrap; }
td { padding: 9px 14px; border-bottom: 1px solid var(--c-border); vertical-align: middle; }
tbody tr:last-child td { border-bottom: 0; }
.proto { white-space: nowrap; font-weight: 500; }
.cell { display: inline-flex; align-items: center; gap: 8px; }
.cell code { font-family: var(--font-mono); font-size: var(--fs-caption); word-break: break-all; }
</style>
