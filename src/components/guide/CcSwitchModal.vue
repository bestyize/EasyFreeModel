<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { openCcSwitchLink } from '@/api/openExternal'
import BaseModal from '@/components/BaseModal.vue'
import { errorMessage } from '@/components/media/format'
import CodeBlock from './CodeBlock.vue'
import SegTabs from './SegTabs.vue'
import { PROVIDER_NAME, ccSwitchEndpoint, ccSwitchLink, type CcSwitchApp } from './snippets'

const props = defineProps<{ modelValue: boolean; input: { origin: string; key: string; model: string } }>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()

const APPS: { id: CcSwitchApp; label: string }[] = [
  { id: 'claude', label: 'Claude Code' },
  { id: 'codex', label: 'Codex' },
]
const app = ref<CcSwitchApp>('claude')
const sending = ref(false)
const sent = ref(false)
const failure = ref<string | null>(null)

watch(
  () => props.modelValue,
  (open) => {
    if (!open) return
    app.value = 'claude'
    sending.value = false
    sent.value = false
    failure.value = null
  },
)
watch(app, () => {
  sent.value = false
  failure.value = null
})

const link = computed(() => ccSwitchLink(app.value, props.input))
const endpoint = computed(() => ccSwitchEndpoint(app.value, props.input.origin))
const appLabel = computed(() => APPS.find((a) => a.id === app.value)?.label ?? '')

async function confirm() {
  if (sending.value) return
  sending.value = true
  failure.value = null
  try {
    await openCcSwitchLink(link.value)
    sent.value = true
  } catch (e) {
    failure.value = `未能打开 CC Switch：${errorMessage(e)}。请确认已安装 CC Switch，或复制下面的链接到浏览器地址栏打开。`
  } finally {
    sending.value = false
  }
}
</script>

<template>
  <BaseModal :model-value="modelValue" title="导入到 CC Switch" width="680px" @update:model-value="emit('update:modelValue', $event)">
    <div class="body" data-testid="import-modal-ccswitch">
      <p class="lead">
        点“确认导入”会唤起本机的 CC Switch，把 {{ PROVIDER_NAME }} 作为一个新的提供商交给它。CC Switch 还会再弹一次自己的确认框，
        里面提示“地址指向本机”属于正常现象，因为网关就运行在这台电脑上。
      </p>

      <div class="pick"><span>导入为</span><SegTabs v-model="app" :options="APPS" label="导入到哪个应用" /></div>

      <dl class="facts" data-testid="cc-facts">
        <div><dt>名称</dt><dd>{{ PROVIDER_NAME }}</dd></div>
        <div><dt>接口地址</dt><dd class="mono">{{ endpoint }}</dd></div>
        <div><dt>模型</dt><dd class="mono">{{ input.model }}</dd></div>
        <div><dt>API 密钥</dt><dd class="mono">{{ input.key }}</dd></div>
      </dl>

      <details class="link">
        <summary>查看导入链接</summary>
        <CodeBlock :text="link" />
      </details>

      <p v-if="sent" class="ok" role="status" data-testid="cc-sent">已交给 CC Switch，请在它弹出的窗口里确认导入 {{ appLabel }}。</p>
      <p v-if="failure" class="problem" role="alert" data-testid="cc-failure">{{ failure }}</p>
    </div>

    <template #footer>
      <template v-if="sent">
        <button class="btn btn-primary" type="button" data-testid="cc-done" @click="emit('update:modelValue', false)">完成</button>
      </template>
      <template v-else>
        <button class="btn" type="button" @click="emit('update:modelValue', false)">取消</button>
        <button class="btn btn-primary" type="button" data-testid="confirm-import" :disabled="sending" @click="confirm">
          {{ sending ? '打开中…' : '确认导入' }}
        </button>
      </template>
    </template>
  </BaseModal>
</template>

<style scoped>
.body { display: flex; flex-direction: column; gap: 14px; }
.lead { margin: 0; color: var(--c-text-2); font-size: var(--fs-small); line-height: 1.7; }
.pick { display: flex; align-items: center; gap: 12px; font-size: var(--fs-small); }
.facts { margin: 0; display: flex; flex-direction: column; gap: 8px; font-size: var(--fs-small); }
.facts div { display: flex; gap: 12px; align-items: baseline; }
.facts dt { flex: none; width: 76px; color: var(--c-text-2); }
.facts dd { margin: 0; min-width: 0; word-break: break-all; }
.mono { font-family: var(--font-mono); font-size: var(--fs-caption); }
.link summary { cursor: pointer; margin-bottom: 8px; color: var(--c-text-2); font-size: var(--fs-small); }
.ok { margin: 0; color: var(--c-success); font-size: var(--fs-small); font-weight: 600; }
.problem { margin: 0; padding: 10px 12px; border-radius: 8px; background: #fdecec; color: #a63232; font-size: var(--fs-small); line-height: 1.6; }
</style>
