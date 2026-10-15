<script setup lang="ts">
import { computed, onActivated, onMounted, ref, watch } from 'vue'
import { RouterLink } from 'vue-router'
import { TriangleAlert } from 'lucide-vue-next'
import CodeBlock from '@/components/guide/CodeBlock.vue'
import CopyButton from '@/components/guide/CopyButton.vue'
import SegTabs from '@/components/guide/SegTabs.vue'
import {
  EXAMPLE_LANGS,
  EXAMPLE_PROTOCOLS,
  KEY_PLACEHOLDER,
  MODEL_PLACEHOLDER,
  endpointRows,
  langsFor,
  requestExample,
  type ExampleLang,
  type ExampleProtocol,
} from '@/components/guide/snippets'
import { useGatewayAddress } from '@/components/guide/useGatewayAddress'
import { errorMessage } from '@/components/media/format'
import { useApiKeysStore } from '@/stores/apiKeys'
import { useConnectStore } from '@/stores/connect'
import { useSettingsStore } from '@/stores/settings'
import { useSystemStore } from '@/stores/system'

const keysStore = useApiKeysStore()
const connect = useConnectStore()
const settings = useSettingsStore()
const system = useSystemStore()
const { running, listen, origin, wildcard } = useGatewayAddress()

// ───────── 网关地址 ─────────

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
      key: connect.fullKey || KEY_PLACEHOLDER,
      model: connect.model.trim() || MODEL_PLACEHOLDER,
    }) ?? '',
)

// ───────── 加载 ─────────

function refresh() {
  void keysStore.load()
  void settings.ensureLoaded()
  void connect.loadModels()
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
    <section class="card section" aria-labelledby="g-addr">
      <h2 id="g-addr" class="section-title">网关地址</h2>

      <div v-if="!running && listen" class="banner warn" role="status" data-testid="stopped-banner">
        <TriangleAlert :size="18" aria-hidden="true" />
        <span>本地网关当前未运行，客户端暂时连不上。</span>
        <button class="btn" type="button" data-testid="start-gateway" :disabled="starting" @click="startGateway">{{ starting ? '启动中…' : '启动网关' }}</button>
      </div>
      <p v-if="startError" class="banner danger" role="alert" data-testid="start-error">{{ startError }}</p>

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

    <section class="card section" aria-labelledby="g-example">
      <h2 id="g-example" class="section-title">请求示例</h2>
      <p class="lead" data-testid="example-source">
        示例里的密钥和模型来自
        <RouterLink to="/api-keys">API 密钥</RouterLink>
        页的“连接参数”，在那里改了这里会跟着变。
      </p>
      <div class="tabrow">
        <SegTabs v-model="protocol" :options="EXAMPLE_PROTOCOLS" label="协议" />
        <SegTabs v-model="lang" :options="langOptions" label="语言" />
      </div>
      <CodeBlock :text="exampleText" :title="lang === 'curl' ? 'shell' : lang" data-testid="example-code" />
    </section>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 16px; }
.section { padding: 20px 24px; display: flex; flex-direction: column; gap: 12px; }
.lead { margin: 0; color: var(--c-text-2); font-size: var(--fs-small); line-height: 1.7; }
.lead a { color: var(--c-brand); text-decoration: none; }
.lead a:hover { text-decoration: underline; }
.hint { margin: 0; padding: 8px 12px; border-radius: 8px; background: var(--c-warning-soft); color: var(--c-warning); font-size: var(--fs-small); line-height: 1.6; }
.muted { color: var(--c-text-2); }
.banner { display: flex; align-items: center; gap: 10px; margin: 0; padding: 10px 14px; border-radius: var(--radius); font-size: var(--fs-small); line-height: 1.6; }
.banner svg { flex: none; }
.banner > span { flex: 1; }
.banner.warn { border: 1px solid var(--c-warning-border); background: var(--c-warning-soft); color: var(--c-warning); }
.banner.danger { border: 1px solid var(--c-danger-border); background: var(--c-danger-soft); color: var(--c-danger); }
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
