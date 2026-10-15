<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { ArrowLeft, ArrowRight, Check, Copy, Laptop, Plus, Sparkles, Terminal, Trash2, Workflow } from 'lucide-vue-next'
import { api } from '@/api'
import { openCcSwitchLink } from '@/api/openExternal'
import BaseModal from '@/components/BaseModal.vue'
import { errorMessage } from '@/components/media/format'
import { useGatewayAddress } from '@/components/guide/useGatewayAddress'
import { ccSwitchLink } from '@/components/guide/snippets'
import type { ApiKeyInfo, ClientImportInput, ClientImportOutcome, ClientImportPreview, ImportClient } from '@/types'

type Target = 'codex' | 'claude' | 'desktop' | 'ccswitch'
const props = defineProps<{ modelValue: boolean; apiKey: ApiKeyInfo | null }>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()
const { origin, running } = useGatewayAddress()
const pickerOpen = ref(false)
const target = ref<Target | null>(null)
const setupOpen = computed({
  get: () => target.value !== null,
  set: (v: boolean) => { if (!v) target.value = null },
})
const modelNames = ref<string[]>([])
const modelError = ref<string | null>(null)
const model = ref('')
const haiku = ref('')
const sonnet = ref('')
const opus = ref('')
const desktopModels = ref<string[]>([])
const ccApp = ref<'claude' | 'codex'>('claude')
const preview = ref<ClientImportPreview | null>(null)
const previewKey = ref('')
const outcome = ref<ClientImportOutcome | null>(null)
const busy = ref(false)
const failure = ref<string | null>(null)
const copied = ref<string | null>(null)
let previewSequence = 0

const choices: { id: Target; name: string; description: string; icon: typeof Terminal }[] = [
  { id: 'codex', name: 'Codex', description: '写入本机 config.toml', icon: Terminal },
  { id: 'claude', name: 'Claude Code', description: '写入本机 settings.json', icon: Sparkles },
  { id: 'desktop', name: 'Claude Desktop', description: '复制参数，在桌面应用内配置', icon: Laptop },
  { id: 'ccswitch', name: 'CC Switch', description: '唤起本机导入确认', icon: Workflow },
]
const label = computed(() => choices.find((x) => x.id === target.value)?.name || '')
const options = computed(() => modelNames.value)
const validModels = computed(() => !!model.value.trim())
const desktopModelList = computed(() => [...new Set([model.value, ...desktopModels.value].map((name) => name.trim()).filter(Boolean))])
const key = computed(() => props.apiKey?.secret || '')
const request = computed<ClientImportInput>(() => ({
  client: target.value as ImportClient,
  origin: origin.value,
  apiKey: key.value,
  model: model.value.trim(),
  haikuModel: haiku.value.trim(),
  sonnetModel: sonnet.value.trim(),
  opusModel: opus.value.trim(),
}))
const requestFingerprint = computed(() => JSON.stringify(request.value))
const canProceed = computed(() => !!key.value && !!origin.value && validModels.value && !busy.value)

watch(() => props.modelValue, async (open) => {
  if (!open) { pickerOpen.value = false; target.value = null; return }
  pickerOpen.value = true
  target.value = null
  modelNames.value = []
  modelError.value = null
  try {
    const list = await api.listGatewayModels()
    if (!props.modelValue) return
    modelNames.value = [...new Set(list.map((entry) => entry.id))]
  } catch (e) { modelError.value = errorMessage(e) }
})
watch(options, (list) => {
  if (!target.value || model.value || !list.length) return
  model.value = list[0]
})

function choose(id: Target) {
  if (!props.apiKey?.secret) return
  pickerOpen.value = false
  model.value = options.value[0] || ''
  haiku.value = ''
  sonnet.value = ''
  opus.value = ''
  desktopModels.value = []
  ccApp.value = 'claude'
  preview.value = null
  previewKey.value = ''
  outcome.value = null
  failure.value = null
  void nextTick(() => { target.value = id })
}
function back() {
  target.value = null
  pickerOpen.value = true
  failure.value = null
  outcome.value = null
}
function closePicker() {
  pickerOpen.value = false
  emit('update:modelValue', false)
}
function closeSetup() {
  target.value = null
  emit('update:modelValue', false)
}
async function loadPreview() {
  const sequence = ++previewSequence
  preview.value = null
  previewKey.value = ''
  failure.value = null
  if (!canProceed.value || (target.value !== 'codex' && target.value !== 'claude')) return
  const fingerprint = requestFingerprint.value
  try {
    const result = await api.previewClientImport(request.value)
    if (sequence === previewSequence) {
      preview.value = result
      previewKey.value = fingerprint
    }
  } catch (e) {
    if (sequence === previewSequence) failure.value = errorMessage(e)
  }
}
watch([target, model, haiku, sonnet, opus, ccApp, origin], () => {
  if (target.value === 'codex' || target.value === 'claude') void loadPreview()
})
async function confirm() {
  if (!canProceed.value || !preview.value || preview.value.error || preview.value.unchanged || previewKey.value !== requestFingerprint.value) return
  busy.value = true
  failure.value = null
  try { outcome.value = await api.applyClientImport(request.value) }
  catch (e) { failure.value = errorMessage(e) }
  finally { busy.value = false }
}
async function importCcSwitch() {
  if (!canProceed.value) return
  busy.value = true
  failure.value = null
  try {
    await openCcSwitchLink(ccSwitchLink(ccApp.value, {
      origin: origin.value, key: key.value, model: model.value.trim(),
      haikuModel: haiku.value.trim(), sonnetModel: sonnet.value.trim(), opusModel: opus.value.trim(),
    }))
    outcome.value = { path: 'CC Switch', backupPath: null, changed: true }
  } catch (e) { failure.value = errorMessage(e) }
  finally { busy.value = false }
}
async function copy(value: string, what: string) {
  try { await navigator.clipboard.writeText(value); copied.value = what }
  catch (e) { failure.value = '复制失败：' + errorMessage(e) }
}
const isSecret = (field: string) => /token|api_key|bearer/i.test(field)
</script>

<template>
  <BaseModal :model-value="pickerOpen" title="接入客户端" width="580px" @update:model-value="!$event && closePicker()">
    <div class="intro">
      <strong>{{ apiKey?.name }}</strong>
      <span>选择要使用这把密钥的本机客户端。供应商名称固定为 FreeApiBox。</span>
    </div>
    <div class="target-list" data-testid="import-target-list">
      <button v-for="item in choices" :key="item.id" type="button" class="target" :data-testid="'pick-' + item.id" @click="choose(item.id)">
        <span class="target-icon"><component :is="item.icon" :size="20" /></span>
        <span class="target-text"><strong>{{ item.name }}</strong><small>{{ item.description }}</small></span>
        <ArrowRight :size="17" />
      </button>
    </div>
    <p class="hint">Codex 与 Claude Code 会先预览改动，确认后备份原配置再写入；CC Switch 会在自己的窗口确认。</p>
  </BaseModal>

  <BaseModal :model-value="setupOpen" :title="'接入 ' + label" width="680px" @update:model-value="!$event && closeSetup()">
    <div class="setup" :data-testid="'setup-' + target">
      <div class="identity">
        <span>供应商 <strong>FreeApiBox</strong></span>
        <span>密钥 <strong>{{ apiKey?.name }}</strong></span>
        <span>本地地址 <code>{{ origin }}</code></span>
      </div>

      <template v-if="outcome">
        <div class="success" role="status"><Check :size="18" />
          <span v-if="target === 'ccswitch'">已唤起 CC Switch，请在它的窗口中确认导入。</span>
          <span v-else>{{ outcome.changed ? '配置已写入。重新打开客户端或新建会话后生效。' : '配置已经是最新。' }}</span>
        </div>
        <p v-if="outcome.path !== 'CC Switch'" class="muted">配置文件：<code>{{ outcome.path }}</code></p>
        <p v-if="outcome.backupPath" class="muted">原文件备份：<code>{{ outcome.backupPath }}</code></p>
      </template>
      <template v-else>
        <p v-if="!running" class="notice">本地网关尚未运行；配置可以先保存，但客户端要等网关启动后才能连接。</p>
        <p v-if="modelError" class="notice">读取模型列表失败：{{ modelError }}。可以手动填写模型名。</p>

        <div v-if="target === 'ccswitch'" class="field">
          <label for="import-cc-app">导入到</label>
          <select id="import-cc-app" v-model="ccApp"><option value="claude">Claude Code</option><option value="codex">Codex</option></select>
        </div>

        <div class="model-heading">
          <strong>模型配置</strong>
          <span>从已启用模型中选择，也可以输入模型名。</span>
        </div>
        <div class="fields">
          <div class="field">
            <label for="import-default-model">默认模型</label>
            <input id="import-default-model" v-model="model" list="import-model-options" placeholder="选择或输入模型名" autocomplete="off" spellcheck="false" />
          </div>
          <template v-if="target === 'claude' || (target === 'ccswitch' && ccApp === 'claude')">
            <div class="field"><label for="import-sonnet">Sonnet 档位</label><input id="import-sonnet" v-model="sonnet" list="import-model-options" placeholder="沿用默认模型" autocomplete="off" spellcheck="false" /></div>
            <div class="field"><label for="import-opus">Opus 档位</label><input id="import-opus" v-model="opus" list="import-model-options" placeholder="沿用默认模型" autocomplete="off" spellcheck="false" /></div>
            <div class="field"><label for="import-haiku">Haiku 档位</label><input id="import-haiku" v-model="haiku" list="import-model-options" placeholder="沿用默认模型" autocomplete="off" spellcheck="false" /></div>
          </template>
          <datalist id="import-model-options"><option v-for="name in options" :key="name" :value="name" /></datalist>
        </div>
        <div v-if="target === 'desktop'" class="desktop-models">
          <p class="hint">Claude Desktop 的模型列表可以加入多个模型，第一个模型作为默认选项。</p>
          <div v-for="(_, index) in desktopModels" :key="index" class="field desktop-model-row">
            <label :for="'import-desktop-model-' + index">额外模型 {{ index + 1 }}</label>
            <div class="model-input-row">
              <input :id="'import-desktop-model-' + index" v-model="desktopModels[index]" list="import-model-options" placeholder="选择或输入模型名" autocomplete="off" spellcheck="false" />
              <button class="btn" type="button" :aria-label="'移除额外模型 ' + (index + 1)" @click="desktopModels.splice(index, 1)"><Trash2 :size="15" /></button>
            </div>
          </div>
          <button class="btn add-model" type="button" data-testid="add-desktop-model" @click="desktopModels.push('')"><Plus :size="15" /> 添加模型</button>
        </div>
        <p v-if="target === 'claude' || (target === 'ccswitch' && ccApp === 'claude')" class="hint">三个档位可分别指定模型；留空时沿用默认模型。</p>
        <p v-if="!options.length" class="hint">还没有已启用的模型。先在“提供商”中启用模型，也可以输入已有的公开模型别名。</p>
        <p v-if="!model.trim()" class="notice">请填写默认模型。</p>

        <template v-if="target === 'desktop'">
          <div class="desktop-guide">
            <strong>Claude Desktop 需要在应用内配置第三方推理</strong>
            <p>打开 Claude Desktop → Help → Troubleshooting → Enable Developer Mode，再打开 Developer → Configure Third-Party Inference。选择 Gateway，填写下方地址与密钥，在 Models 中逐个添加模型并应用更改。明确填写模型列表可让非 Claude 命名的模型出现在选择器中。</p>
            <div><code>{{ origin }}</code><button class="btn" type="button" @click="copy(origin, '地址')"><Copy :size="14" /> {{ copied === '地址' ? '已复制' : '复制地址' }}</button></div>
            <div><code>{{ apiKey?.masked }}</code><button class="btn" type="button" @click="copy(key, '密钥')"><Copy :size="14" /> {{ copied === '密钥' ? '已复制' : '复制密钥' }}</button></div>
            <div v-for="(name, index) in desktopModelList" :key="name" :data-testid="'desktop-copy-model-' + index">
              <code>{{ index === 0 ? '默认：' : '' }}{{ name }}</code>
              <button class="btn" type="button" @click="copy(name, '模型' + index)"><Copy :size="14" /> {{ copied === '模型' + index ? '已复制' : '复制模型' }}</button>
            </div>
            <button v-if="desktopModelList.length > 1" class="btn" type="button" data-testid="copy-desktop-models" @click="copy(desktopModelList.join('\n'), '模型列表')"><Copy :size="14" /> {{ copied === '模型列表' ? '已复制模型列表' : '复制全部模型名' }}</button>
          </div>
        </template>

        <template v-else-if="target === 'codex' || target === 'claude'">
          <div v-if="preview" class="preview">
            <strong>{{ preview.exists ? '将修改已有配置' : '将创建配置文件' }}</strong>
            <code>{{ preview.path }}</code>
            <p v-if="preview.error" class="notice">无法导入：{{ preview.error }}</p>
            <p v-else-if="preview.unchanged" class="muted">配置已是最新。</p>
            <details v-else>
              <summary>查看要修改的字段（{{ preview.changes.length }}）</summary>
              <div v-for="change in preview.changes" :key="change.field" class="change">
                <code>{{ change.field }}</code><span>→ {{ isSecret(change.field) ? '••••' : (change.after ?? '删除') }}</span>
              </div>
            </details>
            <small v-if="preview.exists">写入前会备份原文件；其它字段保持原样。</small>
          </div>
          <p v-else-if="canProceed" class="muted">正在读取客户端配置…</p>
          <p v-if="target === 'codex'" class="hint">Codex 一次使用一个默认模型；其他可用模型仍可通过网关访问。</p>
        </template>
        <p v-else class="hint">确认后会打开 CC Switch 的本机导入窗口。</p>
        <p v-if="failure" class="notice" role="alert">{{ failure }}</p>
      </template>
    </div>
    <template #footer>
      <button v-if="!outcome" class="btn" type="button" @click="back"><ArrowLeft :size="15" /> 返回列表</button>
      <button v-if="outcome || target === 'desktop' || preview?.unchanged" class="btn btn-primary" type="button" @click="closeSetup">完成</button>
      <button v-else-if="target === 'ccswitch'" class="btn btn-primary" type="button" :disabled="!canProceed" @click="importCcSwitch">{{ busy ? '正在打开…' : '导入到 CC Switch' }}</button>
      <button v-else class="btn btn-primary" type="button" data-testid="confirm-import" :disabled="!canProceed || !preview || !!preview.error || preview.unchanged || previewKey !== requestFingerprint" @click="confirm">{{ busy ? '正在导入…' : '确认导入' }}</button>
    </template>
  </BaseModal>
</template>

<style scoped>
.intro { display: flex; flex-direction: column; gap: 5px; margin-bottom: 16px; color: var(--c-text-2); font-size: var(--fs-small); }
.intro strong { color: var(--c-text); font-size: var(--fs-body); }
.target-list { display: grid; gap: 8px; }
.target { display: flex; align-items: center; gap: 13px; width: 100%; padding: 13px 14px; border: 1px solid var(--c-border); border-radius: 10px; background: var(--c-canvas); color: var(--c-text); text-align: left; cursor: pointer; }
.target:hover { border-color: var(--c-brand); background: var(--c-brand-soft); }
.target-icon { display: grid; place-items: center; width: 38px; height: 38px; border-radius: 9px; background: var(--c-surface-2); color: var(--c-brand); }
.target-text { display: flex; flex: 1; flex-direction: column; gap: 3px; }
.target-text small { color: var(--c-text-2); }
.setup { display: flex; flex-direction: column; gap: 14px; }
.identity { display: flex; flex-wrap: wrap; gap: 7px 16px; padding: 11px 13px; border-radius: 8px; background: var(--c-surface-2); color: var(--c-text-2); font-size: var(--fs-small); }
.identity strong,.identity code { color: var(--c-text); }
.model-heading { display: flex; align-items: baseline; flex-wrap: wrap; gap: 4px 12px; font-size: var(--fs-small); }
.model-heading span { color: var(--c-text-2); }
.fields { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
.field { display: flex; flex-direction: column; gap: 6px; font-size: var(--fs-small); }
.field input,.field select { width: 100%; min-width: 0; padding: 9px 10px; border: 1px solid var(--c-border); border-radius: 8px; background: var(--c-canvas); color: var(--c-text); }
.desktop-models { display: flex; flex-direction: column; gap: 10px; }
.desktop-models .hint { margin: 0; }
.model-input-row { display: flex; gap: 8px; }
.model-input-row input { flex: 1; }
.model-input-row button { flex: none; }
.add-model { align-self: flex-start; }
.preview,.desktop-guide { display: flex; flex-direction: column; gap: 9px; padding: 14px; border: 1px solid var(--c-border); border-radius: 9px; font-size: var(--fs-small); }
.preview > code,.desktop-guide code,.muted code { overflow-wrap: anywhere; }
.preview details summary { cursor: pointer; color: var(--c-brand); }
.change { display: flex; gap: 12px; justify-content: space-between; padding: 5px 0; border-bottom: 1px solid var(--c-border); overflow-wrap: anywhere; }
.change span { text-align: right; }
.desktop-guide p { margin: 0; color: var(--c-text-2); line-height: 1.6; }
.desktop-guide > div { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
.desktop-guide > div code { min-width: 0; }
.hint,.muted { margin: 0; color: var(--c-text-2); font-size: var(--fs-small); line-height: 1.6; }
.hint { margin-top: 10px; }
.notice { margin: 0; padding: 9px 11px; border-radius: 8px; background: #fdf6e8; color: #7a5a14; font-size: var(--fs-small); line-height: 1.6; }
.success { display: flex; align-items: center; gap: 8px; color: var(--c-success); }
.btn { display: inline-flex; align-items: center; gap: 5px; }
@media(max-width: 620px) { .fields { grid-template-columns: 1fr; } .desktop-guide > div { flex-wrap: wrap; } }
</style>
