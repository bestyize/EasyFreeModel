<script setup lang="ts">
import { computed, nextTick, ref } from 'vue'
import { ChevronDown, ChevronRight, KeyRound, Link2Off, Lock, Pencil, RefreshCw, Trash2, Zap } from 'lucide-vue-next'
import StatusTag from '@/components/StatusTag.vue'
import ToggleSwitch from './ToggleSwitch.vue'
import { connectionStatus } from './status'
import type { Connection, TestResult } from '@/types'

const props = defineProps<{
  connection: Connection
  index: number
  selected: boolean
  expanded: boolean
  /** 本地新增、尚未完成授权/保存的连接 */
  pending?: boolean
  result?: TestResult
  testing?: boolean
  readonly?: boolean
  /** 正在更换 API Key */
  credentialBusy?: boolean
  /** 最近一次更换 API Key 的结果（文案不含密钥） */
  credentialMessage?: { ok: boolean; text: string } | null
  /** OAuth 连接正在重新授权/断开：禁用这两个按钮，防止重复触发 */
  oauthBusy?: boolean
}>()
const emit = defineEmits<{
  'update:connection': [value: Connection]
  'update:selected': [value: boolean]
  'update:expanded': [value: boolean]
  remove: []
  test: []
  /** 提交新的 API Key；本组件在发出事件的同时立刻清空输入框，不保留明文 */
  'update-credential': [apiKey: string]
  /** OAuth 连接令牌失效后，请求重新授权 */
  reauthorize: []
  /** OAuth 连接的“删除”：断开并删除令牌条目（由父组件确认后调用后端） */
  disconnect: []
}>()

const newKey = ref('')
const canUpdateKey = computed(
  () => props.connection.kind === 'apiKey' && !props.pending && !props.readonly && !props.credentialBusy && newKey.value.trim() !== '',
)
function submitKey() {
  if (!canUpdateKey.value) return
  const k = newKey.value
  newKey.value = ''
  emit('update-credential', k)
}

const labelEl = ref<HTMLInputElement | null>(null)
const status = computed(() => connectionStatus(props.connection, props.pending))
const isOAuth = computed(() => props.connection.kind === 'oauth')
const kindText = computed(() => (isOAuth.value ? 'OAuth' : 'API Key'))
const needsReauth = computed(() => isOAuth.value && props.connection.status === 'needsReauth')
const canTest = computed(() => !props.readonly && !props.pending && !props.testing)

function patch(p: Partial<Connection>) {
  emit('update:connection', { ...props.connection, ...p })
}
function toInt(v: string, fallback: number): number {
  const n = Number(v)
  return Number.isFinite(n) ? Math.trunc(n) : fallback
}
async function edit() {
  emit('update:expanded', true)
  await nextTick()
  labelEl.value?.focus()
}
</script>

<template>
  <li class="conn" :class="{ error: connection.status === 'error', warn: needsReauth }">
    <div class="line">
      <input
        type="checkbox"
        :checked="selected"
        :aria-label="`选择连接 ${connection.accountLabel}`"
        @change="emit('update:selected', ($event.target as HTMLInputElement).checked)"
      />
      <button
        type="button"
        class="icon-btn"
        :aria-expanded="expanded"
        :aria-label="expanded ? `收起 ${connection.accountLabel}` : `展开 ${connection.accountLabel}`"
        @click="emit('update:expanded', !expanded)"
      >
        <component :is="expanded ? ChevronDown : ChevronRight" :size="16" />
      </button>
      <component :is="connection.kind === 'oauth' ? Lock : KeyRound" :size="16" class="kind-ico" aria-hidden="true" />
      <span class="label" :title="connection.accountLabel">{{ connection.accountLabel || '（未命名）' }}</span>
      <StatusTag :tone="status.tone">{{ status.text }}</StatusTag>
      <span class="chip">{{ kindText }}</span>
      <span class="seq" :aria-label="`顺序 ${index + 1}`">#{{ index + 1 }}</span>
      <span class="spacer" />
      <span v-if="testing" class="result" role="status">测试中…</span>
      <span v-else-if="result" class="result" :class="result.ok ? 'ok' : 'bad'" role="status">
        {{ result.ok ? `通过${result.latencyMs != null ? ` ${result.latencyMs}ms` : ''}` : `失败：${result.error ?? '未知错误'}` }}
      </span>
      <button
        type="button"
        class="act"
        :disabled="!canTest"
        :aria-label="`测试连接 ${connection.accountLabel}`"
        :title="pending ? '保存后才能测试' : '测试连接'"
        @click="emit('test')"
      >
        <Zap :size="14" /> 测试
      </button>
      <button type="button" class="act" :disabled="readonly" :aria-label="`编辑连接 ${connection.accountLabel}`" @click="edit">
        <Pencil :size="14" /> 编辑
      </button>
      <button
        v-if="needsReauth"
        type="button"
        class="act reauth"
        :disabled="oauthBusy"
        :aria-label="`重新授权 ${connection.accountLabel}`"
        @click="emit('reauthorize')"
      >
        <RefreshCw :size="14" /> 重新授权
      </button>
      <button
        v-if="isOAuth"
        type="button"
        class="act del"
        :disabled="oauthBusy"
        :aria-label="`断开连接 ${connection.accountLabel}`"
        title="断开：删除令牌条目并移除该连接"
        @click="emit('disconnect')"
      >
        <Link2Off :size="14" /> 断开
      </button>
      <button v-else type="button" class="act del" :disabled="readonly" :aria-label="`删除连接 ${connection.accountLabel}`" @click="emit('remove')">
        <Trash2 :size="14" /> 删除
      </button>
      <ToggleSwitch
        :model-value="connection.enabled"
        :label="`${connection.enabled ? '停用' : '启用'}连接 ${connection.accountLabel}`"
        :disabled="readonly"
        @update:model-value="patch({ enabled: $event })"
      />
    </div>

    <div v-if="expanded" class="detail">
      <label class="field">
        <span>账户名称</span>
        <input
          ref="labelEl"
          class="input"
          :value="connection.accountLabel"
          :disabled="readonly"
          aria-label="账户名称"
          @input="patch({ accountLabel: ($event.target as HTMLInputElement).value })"
        />
      </label>
      <label class="field narrow">
        <span>优先级（越小越先）</span>
        <input
          class="input"
          type="number"
          min="0"
          step="1"
          :value="connection.priority"
          :disabled="readonly"
          aria-label="优先级"
          @input="patch({ priority: toInt(($event.target as HTMLInputElement).value, connection.priority) })"
        />
      </label>
      <label class="field narrow">
        <span>权重</span>
        <input
          class="input"
          type="number"
          min="1"
          step="1"
          :value="connection.weight"
          :disabled="readonly"
          aria-label="权重"
          @input="patch({ weight: Math.max(1, toInt(($event.target as HTMLInputElement).value, connection.weight)) })"
        />
      </label>
      <div v-if="connection.kind === 'apiKey' && !pending" class="field key-field">
        <span>更换 API Key</span>
        <div class="key-row">
          <input
            v-model="newKey"
            class="input"
            type="password"
            autocomplete="new-password"
            spellcheck="false"
            placeholder="输入新的 API Key（保存后旧密钥即被清除）"
            :disabled="readonly || credentialBusy"
            aria-label="新的 API Key"
            @keydown.enter.prevent="submitKey"
          />
          <button type="button" class="btn" :disabled="!canUpdateKey" aria-label="更新 API Key" @click="submitKey">
            {{ credentialBusy ? '更新中…' : '更新密钥' }}
          </button>
        </div>
        <span v-if="credentialMessage" class="key-msg" :class="credentialMessage.ok ? 'ok' : 'bad'" role="status">
          {{ credentialMessage.text }}
        </span>
      </div>
      <p v-if="pending" class="note">该连接尚未完成凭据授权，需后端授权流程完成后才可用。</p>
    </div>
  </li>
</template>

<style scoped>
.conn { list-style: none; border: 1px solid var(--c-border); border-radius: var(--radius); background: var(--c-canvas); }
.conn.error { border-color: #f3c3c3; }
.conn.warn { border-color: #f6d9a3; }
.line { display: flex; align-items: center; gap: 8px; min-height: 52px; padding: 6px 12px; }
.line input[type='checkbox'] { width: 16px; height: 16px; accent-color: var(--c-brand); flex: none; }
.icon-btn { display: grid; place-items: center; width: 32px; height: 32px; border: 0; border-radius: 8px; background: transparent; color: var(--c-text-2); }
.icon-btn:hover { background: var(--c-surface-2); }
.kind-ico { color: var(--c-text-2); flex: none; }
.label { font-size: 14px; font-weight: 500; max-width: 240px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.chip { display: inline-flex; align-items: center; height: 22px; padding: 0 8px; border-radius: 11px; background: var(--c-surface-2); font-size: 12px; color: var(--c-text-2); }
.seq { color: var(--c-text-2); font-size: 12px; }
.spacer { flex: 1; }
.result { font-size: 12px; max-width: 220px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--c-text-2); }
.result.ok { color: var(--c-success); }
.result.bad { color: var(--c-danger); }
.act { display: inline-flex; align-items: center; gap: 4px; min-width: 32px; height: 32px; padding: 0 8px; border: 0; border-radius: 8px; background: transparent; color: var(--c-text-2); font-size: 12px; }
.act:hover:not(:disabled) { background: var(--c-surface-2); }
.act:disabled { opacity: 0.45; cursor: not-allowed; }
.act.del { color: var(--c-danger); }
.act.reauth { color: var(--c-brand); font-weight: 500; }
.detail { display: flex; flex-wrap: wrap; gap: 12px; padding: 4px 16px 14px 56px; border-top: 1px dashed var(--c-border); padding-top: 12px; }
.field { display: flex; flex-direction: column; gap: 4px; flex: 1 1 240px; font-size: 12px; color: var(--c-text-2); }
.field.narrow { flex: 0 0 150px; }
.key-field { flex: 1 1 100%; }
.key-row { display: flex; gap: 8px; }
.key-row .input { flex: 1; }
.key-msg.ok { color: var(--c-success); }
.key-msg.bad { color: var(--c-danger); }
.note { flex-basis: 100%; margin: 0; font-size: 12px; color: var(--c-text-2); }
</style>
