<script setup lang="ts">
import { computed, onActivated, onMounted, ref, type Component } from 'vue'
import { ArrowLeftRight, Sparkles, Terminal } from 'lucide-vue-next'
import { useConnectStore } from '@/stores/connect'
import { useSettingsStore } from '@/stores/settings'
import CcSwitchModal from './CcSwitchModal.vue'
import ClientImportModal from './ClientImportModal.vue'
import CopyButton from './CopyButton.vue'
import { useGatewayAddress } from './useGatewayAddress'

const connect = useConnectStore()
const settings = useSettingsStore()
const { origin } = useGatewayAddress()

type Client = 'codex' | 'claude' | 'ccswitch'
const CLIENTS: { id: Client; label: string; desc: string; icon: Component }[] = [
  { id: 'codex', label: 'Codex', desc: '写入 ~/.codex/config.toml', icon: Terminal },
  { id: 'claude', label: 'Claude Code', desc: '写入 ~/.claude/settings.json', icon: Sparkles },
  { id: 'ccswitch', label: 'CC Switch', desc: '交给 CC Switch 管理', icon: ArrowLeftRight },
]
const canImport = computed(() => !!origin.value && !!connect.fullKey && !!connect.model.trim())
const importInput = computed(() => ({ origin: origin.value, key: connect.fullKey, model: connect.model.trim() }))

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

function refresh() {
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
  <div class="connect">
    <section class="card section" aria-labelledby="g-params">
      <h2 id="g-params" class="section-title">连接参数</h2>

      <div v-if="connect.usableKeys.length === 0" class="empty" data-testid="no-key">
        <p>{{ connect.legacyOnly ? '已有的密钥没有保存明文，没法直接用于接入。' : '还没有可用的 API 密钥。' }}</p>
        <button class="btn btn-primary" type="button" data-testid="create-key" :disabled="connect.creating" @click="connect.createKey()">
          {{ connect.creating ? '创建中…' : '一键创建密钥' }}
        </button>
      </div>
      <p v-if="connect.createError" class="warn-text" role="alert" data-testid="create-error">{{ connect.createError }}</p>

      <div class="params">
        <div v-if="connect.usableKeys.length" class="field">
          <label for="g-key">API 密钥</label>
          <select id="g-key" v-model="connect.keyId" class="input" data-testid="key-select">
            <option v-for="k in connect.usableKeys" :key="k.id" :value="k.id">{{ k.name }}</option>
          </select>
          <div class="keyline">
            <code data-testid="key-full">{{ connect.fullKey }}</code>
            <CopyButton :text="connect.fullKey" label="复制密钥" />
          </div>
        </div>

        <div class="field">
          <label for="g-model">模型</label>
          <input
            id="g-model" v-model="connect.model" class="input" list="g-model-list" spellcheck="false" autocomplete="off"
            data-testid="model-input" placeholder="例如 openai/gpt-4o-mini"
          />
          <datalist id="g-model-list"><option v-for="m in connect.models" :key="m.id" :value="m.id">{{ m.providerName }}</option></datalist>
          <p v-if="connect.modelsError" class="field-note warn-text" role="alert">读取模型列表失败：{{ connect.modelsError }}</p>
          <p v-else-if="connect.models.length === 0" class="field-note" data-testid="no-models">
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

    <ClientImportModal v-model="codexOpen" client="codex" :input="importInput" />
    <ClientImportModal v-model="claudeOpen" client="claude" :input="importInput" />
    <CcSwitchModal v-model="ccSwitchOpen" :input="importInput" />
  </div>
</template>

<style scoped>
.connect { display: flex; flex-direction: column; gap: 16px; }
.section { padding: 20px 24px; display: flex; flex-direction: column; gap: 12px; }
.lead { margin: 0; color: var(--c-text-2); font-size: var(--fs-small); line-height: 1.7; }
.empty { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 12px 14px; border: 1px dashed var(--c-border); border-radius: 8px; }
.empty p { margin: 0; font-size: var(--fs-small); }
.params { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 16px; align-items: start; }
.field { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
.field label { font-size: var(--fs-small); font-weight: 500; }
.keyline { display: flex; align-items: center; gap: 8px; min-width: 0; }
.keyline code { flex: 1; min-width: 0; padding: 6px 10px; border-radius: 6px; background: var(--c-surface-2); font-family: var(--font-mono); font-size: var(--fs-caption); word-break: break-all; user-select: all; }
.field-note { margin: 0; color: var(--c-text-2); font-size: var(--fs-caption); line-height: 1.6; }
.warn-text { margin: 0; color: var(--c-warning); font-size: var(--fs-small); line-height: 1.6; }
.field-note.warn-text { font-size: var(--fs-caption); }
.clients { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 12px; }
.client { display: flex; align-items: center; gap: 12px; padding: 14px 16px; border: 1px solid var(--c-border); border-radius: var(--radius); background: var(--c-canvas); color: var(--c-text); text-align: left; }
.client:hover:not(:disabled) { border-color: var(--c-brand); background: var(--c-brand-soft); }
.client:disabled { opacity: 0.5; cursor: not-allowed; }
.client svg { flex: none; color: var(--c-brand); }
.c-text { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.c-text strong { font-size: var(--fs-body); }
.c-text small { color: var(--c-text-2); font-size: var(--fs-caption); font-family: var(--font-mono); }
</style>
