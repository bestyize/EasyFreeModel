<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { api } from '@/api'
import BaseModal from '@/components/BaseModal.vue'
import StatusTag from '@/components/StatusTag.vue'
import { errorMessage } from '@/components/media/format'
import type { ClientImportOutcome, ClientImportPreview, ImportClient } from '@/types'

const props = defineProps<{
  modelValue: boolean
  client: ImportClient
  input: { origin: string; key: string; model: string }
}>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()

const LABELS: Record<ImportClient, string> = { codex: 'Codex', claude: 'Claude Code' }
const label = computed(() => LABELS[props.client])

const loading = ref(false)
const saving = ref(false)
const preview = ref<ClientImportPreview | null>(null)
const failure = ref<string | null>(null)
const outcome = ref<ClientImportOutcome | null>(null)

const request = computed(() => ({
  client: props.client,
  origin: props.input.origin,
  apiKey: props.input.key,
  model: props.input.model,
}))

async function load() {
  loading.value = true
  preview.value = null
  failure.value = null
  outcome.value = null
  try {
    preview.value = await api.previewClientImport(request.value)
  } catch (e) {
    failure.value = errorMessage(e)
  } finally {
    loading.value = false
  }
}

watch(
  () => props.modelValue,
  (open) => {
    if (open) void load()
  },
  { immediate: true },
)

const blocked = computed(() => loading.value || saving.value || !preview.value || !!preview.value.error || preview.value.unchanged)

async function confirm() {
  if (blocked.value) return
  saving.value = true
  failure.value = null
  try {
    outcome.value = await api.applyClientImport(request.value)
  } catch (e) {
    failure.value = `导入失败：${errorMessage(e)}`
  } finally {
    saving.value = false
  }
}

const fileState = computed(() => {
  const p = preview.value
  if (!p) return null
  if (p.unchanged) return { text: '已是最新', tone: 'success' as const }
  return p.exists ? { text: '修改现有文件', tone: 'warning' as const } : { text: '新建文件', tone: 'brand' as const }
})
const restartHint = computed(() => (props.client === 'codex' ? '重启 Codex（或新开一个会话）后生效。' : '新开一个 Claude Code 会话后生效。'))
</script>

<template>
  <BaseModal :model-value="modelValue" :title="`导入到 ${label}`" width="680px" @update:model-value="emit('update:modelValue', $event)">
    <div class="body" :data-testid="`import-modal-${client}`">
      <p v-if="loading" class="muted">正在读取现有配置…</p>

      <template v-else-if="outcome">
        <p class="ok" role="status" data-testid="import-done">
          {{ outcome.changed ? `已导入到 ${label}。` : '配置本来就是最新，没有改动。' }}{{ outcome.changed ? restartHint : '' }}
        </p>
        <dl class="facts">
          <div><dt>配置文件</dt><dd class="mono">{{ outcome.path }}</dd></div>
          <div v-if="outcome.backupPath"><dt>原文件备份</dt><dd class="mono" data-testid="backup-path">{{ outcome.backupPath }}</dd></div>
        </dl>
      </template>

      <template v-else-if="preview">
        <p class="lead">
          将把网关地址、密钥和模型写入 {{ label }} 的配置文件。只改下面列出的字段，文件里的其它内容保持原样。
        </p>
        <dl class="facts">
          <div>
            <dt>配置文件</dt>
            <dd>
              <span class="mono" data-testid="import-path">{{ preview.path }}</span>
              <StatusTag v-if="fileState" :tone="fileState.tone" data-testid="file-state">{{ fileState.text }}</StatusTag>
            </dd>
          </div>
        </dl>

        <p v-if="preview.error" class="problem" role="alert" data-testid="preview-error">无法导入：{{ preview.error }}</p>

        <table v-else class="changes" data-testid="changes">
          <thead><tr><th>字段</th><th>当前值</th><th>改为</th></tr></thead>
          <tbody>
            <tr v-for="c in preview.changes" :key="c.field" :class="{ same: c.before === c.after }">
              <td class="mono field">{{ c.field }}</td>
              <td class="mono" :class="{ none: c.before === null }">{{ c.before ?? '（无）' }}</td>
              <td class="mono" :class="{ none: c.after === null, del: c.after === null }">
                {{ c.before === c.after ? '不变' : (c.after ?? '（删除）') }}
              </td>
            </tr>
          </tbody>
        </table>

        <ul v-if="!preview.error && !preview.unchanged" class="notes">
          <li v-if="preview.exists">写入前会自动备份原文件（同目录下的 <code>*.router-bak-时间</code>）。</li>
          <li v-if="client === 'codex'">会把 Codex 当前使用的提供商切换为 <code>router</code>；原来的提供商配置都还在，随时可以切回去。</li>
          <li>{{ restartHint }}</li>
        </ul>
        <p v-if="preview.unchanged" class="muted" data-testid="unchanged-note">{{ label }} 的配置已经是这些值，不需要再导入。</p>
      </template>

      <p v-if="failure" class="problem" role="alert" data-testid="import-failure">{{ failure }}</p>
    </div>

    <template #footer>
      <template v-if="outcome">
        <button class="btn btn-primary" type="button" data-testid="done" @click="emit('update:modelValue', false)">完成</button>
      </template>
      <template v-else>
        <button class="btn" type="button" @click="emit('update:modelValue', false)">{{ preview?.unchanged ? '关闭' : '取消' }}</button>
        <button v-if="!preview?.unchanged" class="btn btn-primary" type="button" data-testid="confirm-import" :disabled="blocked" @click="confirm">
          {{ saving ? '导入中…' : '确认导入' }}
        </button>
      </template>
    </template>
  </BaseModal>
</template>

<style scoped>
.body { display: flex; flex-direction: column; gap: 12px; }
.lead { margin: 0; color: var(--c-text-2); font-size: var(--fs-small); line-height: 1.7; }
.muted { margin: 0; color: var(--c-text-2); font-size: var(--fs-small); }
.ok { margin: 0; color: var(--c-success); font-size: var(--fs-body); font-weight: 600; }
.problem { margin: 0; padding: 10px 12px; border-radius: 8px; background: #fdecec; color: #a63232; font-size: var(--fs-small); line-height: 1.6; }
.facts { margin: 0; display: flex; flex-direction: column; gap: 8px; font-size: var(--fs-small); }
.facts div { display: flex; gap: 12px; align-items: baseline; }
.facts dt { flex: none; width: 76px; color: var(--c-text-2); }
.facts dd { margin: 0; display: flex; flex-wrap: wrap; align-items: center; gap: 8px; min-width: 0; word-break: break-all; }
.mono { font-family: var(--font-mono); font-size: var(--fs-caption); }
.changes { width: 100%; border-collapse: collapse; border: 1px solid var(--c-border); }
.changes th { padding: 7px 10px; text-align: left; color: var(--c-text-2); font-weight: 500; font-size: var(--fs-caption); background: var(--c-sidebar); border-bottom: 1px solid var(--c-border); }
.changes td { padding: 7px 10px; border-bottom: 1px solid var(--c-border); vertical-align: top; word-break: break-all; }
.changes tbody tr:last-child td { border-bottom: 0; }
.changes .field { color: var(--c-text-2); white-space: nowrap; word-break: normal; }
.changes .none { color: var(--c-text-2); }
.changes .del { color: var(--c-danger); }
.changes tr.same td { color: var(--c-text-2); }
.notes { margin: 0; padding-left: 18px; color: var(--c-text-2); font-size: var(--fs-small); line-height: 1.8; }
.notes code { font-family: var(--font-mono); padding: 1px 5px; border-radius: 4px; background: var(--c-surface-2); color: var(--c-text); font-size: var(--fs-caption); }
</style>
