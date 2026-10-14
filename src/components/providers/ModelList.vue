<script setup lang="ts">
import { computed, ref } from 'vue'
import { Ban, Bot, Check, Copy, Power, RefreshCw, Search } from 'lucide-vue-next'
import type { ModelEntry, ProviderDetail } from '@/types'

const props = defineProps<{
  models: ModelEntry[]
  thinkingLevel: ProviderDetail['thinkingLevel']
  /** 处于别名冲突中的模型 ID */
  conflictIds: Set<string>
  readonly?: boolean
  /** 正在拉取模型（顶部按钮显示加载态） */
  fetching?: boolean
}>()
const emit = defineEmits<{
  'update:models': [value: ModelEntry[]]
  'update:thinkingLevel': [value: ProviderDetail['thinkingLevel']]
  /** 请求从上游/内置模型库拉取模型 */
  fetch: []
}>()

const query = ref('')
const copiedId = ref<string | null>(null)
let copiedTimer: ReturnType<typeof setTimeout> | undefined

const visible = computed(() => {
  const q = query.value.trim().toLowerCase()
  if (!q) return props.models
  return props.models.filter(
    (m) =>
      m.publicAlias.toLowerCase().includes(q) ||
      m.upstreamModelId.toLowerCase().includes(q) ||
      m.description.toLowerCase().includes(q),
  )
})
const enabledCount = computed(() => props.models.filter((m) => m.enabled).length)

function patch(id: string, p: Partial<ModelEntry>) {
  emit('update:models', props.models.map((m) => (m.id === id ? { ...m, ...p } : m)))
}
/** 批量操作只作用于当前筛选结果 */
function setAll(enabled: boolean) {
  const ids = new Set(visible.value.map((m) => m.id))
  emit('update:models', props.models.map((m) => (ids.has(m.id) ? { ...m, enabled } : m)))
}
async function copy(m: ModelEntry) {
  const text = m.publicAlias
  try {
    await navigator.clipboard.writeText(text)
  } catch {
    const ta = document.createElement('textarea')
    ta.value = text
    document.body.appendChild(ta)
    ta.select()
    try {
      document.execCommand('copy')
    } finally {
      ta.remove()
    }
  }
  copiedId.value = m.id
  clearTimeout(copiedTimer)
  copiedTimer = setTimeout(() => (copiedId.value = null), 1500)
}
</script>

<template>
  <section class="panel" aria-label="可用模型">
    <div class="bar">
      <h3>可用模型 <small>{{ enabledCount }}/{{ models.length }} 已启用</small></h3>
      <select
        class="input select"
        :value="thinkingLevel"
        :disabled="readonly"
        aria-label="Thinking 档位"
        @change="emit('update:thinkingLevel', ($event.target as HTMLSelectElement).value as ProviderDetail['thinkingLevel'])"
      >
        <option value="auto">Thinking: Auto</option>
        <option value="low">Thinking: Low</option>
        <option value="medium">Thinking: Medium</option>
        <option value="high">Thinking: High</option>
      </select>
      <label class="search">
        <Search :size="14" aria-hidden="true" />
        <input v-model="query" type="search" placeholder="搜索模型…" aria-label="搜索模型" />
      </label>
      <button type="button" class="btn" :disabled="readonly || !visible.length" aria-label="全部激活（当前筛选结果）" @click="setAll(true)">
        <Power :size="14" /> 全部激活
      </button>
      <button type="button" class="btn" :disabled="readonly || !visible.length" aria-label="全部禁用（当前筛选结果）" @click="setAll(false)">
        <Ban :size="14" /> 全部禁用
      </button>
      <button
        type="button"
        class="btn"
        :disabled="readonly || fetching"
        data-testid="fetch-models"
        :aria-label="fetching ? '正在拉取模型' : '从上游拉取模型列表'"
        :title="'向上游 /models 拉取；失败时回退到内置预设模型库'"
        @click="emit('fetch')"
      >
        <RefreshCw :size="14" :class="{ spin: fetching }" /> {{ fetching ? '拉取中…' : '拉取模型' }}
      </button>
    </div>

    <p v-if="!models.length" class="empty">该提供商暂无可用模型。点击「拉取模型」从上游或内置模型库获取。</p>
    <p v-else-if="!visible.length" class="empty">没有匹配“{{ query }}”的模型。</p>
    <ul v-else class="grid">
      <li v-for="m in visible" :key="m.id" class="model" :class="{ conflict: conflictIds.has(m.id), off: !m.enabled }">
        <input
          type="checkbox"
          :checked="m.enabled"
          :disabled="readonly"
          :aria-label="`启用模型 ${m.publicAlias || m.upstreamModelId}`"
          @change="patch(m.id, { enabled: ($event.target as HTMLInputElement).checked })"
        />
        <Bot :size="18" class="bot" aria-hidden="true" />
        <div class="info">
          <input
            class="alias"
            :class="{ bad: conflictIds.has(m.id) }"
            :value="m.publicAlias"
            :disabled="readonly"
            :aria-label="`公开别名 ${m.upstreamModelId}`"
            :aria-invalid="conflictIds.has(m.id) || undefined"
            spellcheck="false"
            @input="patch(m.id, { publicAlias: ($event.target as HTMLInputElement).value })"
          />
          <div class="sub">
            <span class="desc" :title="`上游模型：${m.upstreamModelId}`">{{ m.description || m.upstreamModelId }}</span>
            <span v-if="m.supportsThinking" class="tag think">Thinking</span>
            <span v-else class="tag nothink" title="该模型不支持思考档位">不支持思考</span>
          </div>
          <p v-if="conflictIds.has(m.id)" class="err" role="alert">别名与其它已启用模型冲突，请修改</p>
        </div>
        <button type="button" class="copy" :aria-label="`复制模型 ID ${m.publicAlias}`" @click="copy(m)">
          <component :is="copiedId === m.id ? Check : Copy" :size="16" />
        </button>
      </li>
    </ul>
  </section>
</template>

<style scoped>
.panel { border: 1px solid var(--c-border); border-radius: var(--radius); padding: 14px 16px 16px; background: var(--c-canvas); }
.bar { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; margin-bottom: 12px; }
h3 { margin: 0 auto 0 0; font-size: 15px; font-weight: 600; }
h3 small { margin-left: 6px; font-size: 12px; font-weight: 400; color: var(--c-text-2); }
.select { min-width: 140px; }
.search { display: flex; align-items: center; gap: 6px; width: 180px; height: var(--control-h); padding: 0 10px; border: 1px solid var(--c-border); border-radius: 8px; color: var(--c-text-2); }
.search input { flex: 1; min-width: 0; border: 0; outline: 0; background: transparent; color: var(--c-text); }
.search:focus-within { border-color: var(--c-brand); }
.empty { margin: 8px 0 0; font-size: 13px; color: var(--c-text-2); }
.grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 8px; margin: 0; padding: 0; list-style: none; }
.model { display: flex; align-items: flex-start; gap: 8px; padding: 8px 10px; border: 1px solid var(--c-border); border-radius: 8px; min-width: 0; }
.model.off { background: var(--c-sidebar); }
.model.conflict { border-color: var(--c-danger); background: #fff7f7; }
.model > input[type='checkbox'] { width: 16px; height: 16px; margin-top: 8px; accent-color: var(--c-brand); flex: none; }
.bot { margin-top: 7px; color: var(--c-text-2); flex: none; }
.info { flex: 1; min-width: 0; }
.alias { width: 100%; height: 28px; padding: 0 6px; border: 1px solid transparent; border-radius: 6px; background: transparent; font-size: 13px; font-weight: 600; }
.alias:hover:not(:disabled) { border-color: var(--c-border); }
.alias:focus { border-color: var(--c-brand); background: var(--c-canvas); }
.alias.bad { border-color: var(--c-danger); }
.sub { display: flex; align-items: center; gap: 6px; padding: 0 6px; font-size: 12px; color: var(--c-text-2); min-width: 0; }
.desc { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tag { flex: none; padding: 0 6px; border-radius: 8px; font-size: 11px; line-height: 16px; }
.think { color: var(--c-brand); background: var(--c-brand-soft); }
.nothink { color: var(--c-text-2); background: var(--c-surface-2); }
.err { margin: 2px 6px 0; font-size: 12px; color: var(--c-danger); }
.copy { display: grid; place-items: center; width: 32px; height: 32px; flex: none; border: 0; border-radius: 8px; background: transparent; color: var(--c-text-2); }
.copy:hover { background: var(--c-surface-2); }
.spin { animation: spin 1s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
</style>
