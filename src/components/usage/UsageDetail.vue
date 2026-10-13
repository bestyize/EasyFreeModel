<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { Download, RotateCcw } from 'lucide-vue-next'
import EmptyState from '@/components/EmptyState.vue'
import { useUsageStore } from '@/stores/usage'
import type { Protocol, UsageRequest } from '@/types'
import RequestTable from './RequestTable.vue'
import { downloadCsv, requestsToCsv } from './csv'
import { RANGE_OPTIONS } from './format'

const store = useUsageStore()

const STATUS_OPTIONS: { value: '' | UsageRequest['status']; label: string }[] = [
  { value: '', label: '全部状态' },
  { value: 'success', label: '成功' },
  { value: 'error', label: '失败' },
  { value: 'cancelled', label: '已取消' },
]
const PROTOCOL_OPTIONS: { value: '' | Protocol; label: string }[] = [
  { value: '', label: '全部协议' },
  { value: 'openai-chat', label: 'OpenAI Chat' },
  { value: 'openai-responses', label: 'OpenAI Responses' },
  { value: 'anthropic-messages', label: 'Anthropic Messages' },
  { value: 'gemini', label: 'Gemini' },
]

// 文本输入防抖，避免每敲一个字符就请求一次
const queryInput = ref(store.filter.query)
const modelInput = ref(store.filter.model)
let debounce: ReturnType<typeof setTimeout> | undefined
function scheduleText() {
  clearTimeout(debounce)
  debounce = setTimeout(() => void store.setFilter({ query: queryInput.value, model: modelInput.value }), 300)
}
watch([queryInput, modelInput], scheduleText)
onBeforeUnmount(() => clearTimeout(debounce))

const rangeLabel = computed(() => RANGE_OPTIONS.find((r) => r.value === store.range)?.label ?? '')
const filtersActive = computed(() => {
  const f = store.filter
  return Boolean(f.providerId || f.model.trim() || f.status || f.protocol || f.query.trim())
})

function reset() {
  clearTimeout(debounce)
  queryInput.value = ''
  modelInput.value = ''
  void store.resetFilter()
}

function pad(n: number) {
  return String(n).padStart(2, '0')
}

function exportCsv() {
  const d = new Date()
  const stamp = `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}-${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`
  downloadCsv(`usage-requests-${store.range}-${stamp}.csv`, requestsToCsv(store.detailRows))
}
</script>

<template>
  <section class="card detail" aria-label="请求明细">
    <div class="filters">
      <span class="range-note" title="在右上角切换时间维度">时间：{{ rangeLabel }}</span>
      <select class="input" aria-label="提供商" :value="store.filter.providerId" @change="store.setFilter({ providerId: ($event.target as HTMLSelectElement).value })">
        <option value="">全部提供商</option>
        <option v-for="p in store.providerOptions" :key="p.id" :value="p.id">{{ p.name }}</option>
      </select>
      <input v-model="modelInput" class="input" list="usage-model-options" placeholder="模型" aria-label="模型" />
      <datalist id="usage-model-options">
        <option v-for="m in store.modelOptions" :key="m" :value="m" />
      </datalist>
      <select class="input" aria-label="状态" :value="store.filter.status" @change="store.setFilter({ status: ($event.target as HTMLSelectElement).value as '' | UsageRequest['status'] })">
        <option v-for="o in STATUS_OPTIONS" :key="o.value" :value="o.value">{{ o.label }}</option>
      </select>
      <select class="input" aria-label="协议" :value="store.filter.protocol" @change="store.setFilter({ protocol: ($event.target as HTMLSelectElement).value as '' | Protocol })">
        <option v-for="o in PROTOCOL_OPTIONS" :key="o.value" :value="o.value">{{ o.label }}</option>
      </select>
      <input v-model="queryInput" class="input query" type="search" placeholder="请求 ID / 账号" aria-label="搜索请求" />
      <span class="spacer" />
      <button v-if="filtersActive" class="btn" type="button" @click="reset"><RotateCcw :size="14" />重置</button>
      <button class="btn" type="button" :disabled="store.detailRows.length === 0" data-test="export" @click="exportCsv">
        <Download :size="14" />导出 CSV
      </button>
    </div>

    <p v-if="store.detailStatus === 'loading' && store.detailRows.length === 0" class="state" role="status">加载中…</p>
    <EmptyState v-else-if="store.detailStatus === 'error' && store.detailRows.length === 0" title="请求明细加载失败" :description="store.detailError ?? ''">
      <button class="btn btn-primary" type="button" @click="store.load(true)">重试</button>
    </EmptyState>
    <template v-else>
      <p v-if="store.detailStatus === 'error'" class="banner" role="alert">
        刷新失败：{{ store.detailError }}（显示的是上次结果）
        <button class="btn" type="button" @click="store.load(true)">重试</button>
      </p>
      <EmptyState v-if="store.detailRows.length === 0" :title="filtersActive ? '没有符合条件的请求' : '该时间范围暂无请求'">
        <button v-if="filtersActive" class="btn" type="button" @click="reset">重置筛选</button>
      </EmptyState>
      <div v-else :class="{ stale: store.detailStatus === 'loading' }" :aria-busy="store.detailStatus === 'loading'">
        <RequestTable :rows="store.detailRows" :provider-names="store.providerNames" :page-size="25" />
      </div>
    </template>
  </section>
</template>

<style scoped>
.detail { padding: 14px 16px; }
.filters { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin-bottom: 12px; }
.range-note { padding: 0 10px; height: var(--control-h); display: inline-flex; align-items: center; border-radius: 8px; background: var(--c-surface-2); color: var(--c-text-2); font-size: var(--fs-small); }
.filters .input { min-width: 0; font-size: var(--fs-small); }
.filters input.input { width: 140px; }
.filters .query { width: 170px; }
.spacer { flex: 1; }
.state { margin: 0; padding: 40px 0; text-align: center; color: var(--c-text-2); }
.banner { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin: 0 0 10px; padding: 8px 12px; border-radius: 8px; background: #fdecec; color: var(--c-danger); font-size: var(--fs-small); }
.stale { opacity: 0.6; }
</style>
