<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { ChevronLeft, ChevronRight } from 'lucide-vue-next'
import type { UsageRequest } from '@/types'
import StatusTag, { type StatusTone } from '@/components/StatusTag.vue'
import { formatDateTime, formatInt, formatCost, formatLatency, formatRelative } from './format'

const props = withDefaults(
  defineProps<{
    rows: UsageRequest[]
    /** providerId -> 显示名；缺失时回退为 id */
    providerNames?: Record<string, string>
    /** 紧凑模式（概览页右侧）：少列、相对时间、不分页 */
    compact?: boolean
    /** 每页行数；仅非紧凑模式分页，避免长列表一次渲染几千行 */
    pageSize?: number
  }>(),
  { providerNames: () => ({}), compact: false, pageSize: 25 },
)

const page = ref(1)
const pageCount = computed(() => (props.compact ? 1 : Math.max(1, Math.ceil(props.rows.length / props.pageSize))))
const visible = computed(() => {
  if (props.compact) return props.rows
  const start = (page.value - 1) * props.pageSize
  return props.rows.slice(start, start + props.pageSize)
})

// 数据集变化（换筛选/刷新）后回到第一页；总页数缩小时夹紧
watch(
  () => props.rows,
  () => (page.value = 1),
)
watch(pageCount, (n) => {
  if (page.value > n) page.value = n
})

const STATUS: Record<UsageRequest['status'], { text: string; tone: StatusTone }> = {
  success: { text: '成功', tone: 'success' },
  error: { text: '失败', tone: 'danger' },
  cancelled: { text: '已取消', tone: 'neutral' },
}

function providerName(id: string): string {
  return props.providerNames[id] ?? id
}
</script>

<template>
  <div class="request-table" :class="{ compact }">
    <div class="scroll">
      <table>
        <thead>
          <tr v-if="compact">
            <th>模型</th>
            <th>Token（入/出）</th>
            <th>提供商</th>
            <th>时间</th>
            <th>状态</th>
          </tr>
          <tr v-else>
            <th>时间</th>
            <th>请求 ID</th>
            <th>提供商</th>
            <th>账号</th>
            <th>模型</th>
            <th class="num">输入</th>
            <th class="num">输出</th>
            <th class="num">缓存</th>
            <th class="num">延迟</th>
            <th class="num">成本</th>
            <th>状态</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="r in visible" :key="r.requestId" data-test="row">
            <template v-if="compact">
              <td class="model" :title="r.model">{{ r.model }}</td>
              <td class="tok">
                <span class="in">↑{{ formatInt(r.inputTokens) }}</span>
                <span class="out">↓{{ formatInt(r.outputTokens) }}</span>
                <StatusTag v-if="r.tokenSource === 'estimated'" tone="warning" :dot="false" title="Token 数为本地估算">估算</StatusTag>
              </td>
              <td>{{ providerName(r.providerId) }}</td>
              <td class="time" :title="formatDateTime(r.timestamp)">{{ formatRelative(r.timestamp) }}</td>
              <td><StatusTag :tone="STATUS[r.status].tone">{{ STATUS[r.status].text }}</StatusTag></td>
            </template>
            <template v-else>
              <td class="time">{{ formatDateTime(r.timestamp) }}</td>
              <td class="mono" :title="r.requestId">{{ r.requestId }}</td>
              <td>{{ providerName(r.providerId) }}</td>
              <td class="mono" :title="r.accountId">{{ r.accountId }}</td>
              <td class="model" :title="r.model">{{ r.model }}</td>
              <td class="num">
                {{ formatInt(r.inputTokens) }}
                <StatusTag v-if="r.tokenSource === 'estimated'" tone="warning" :dot="false" title="Token 数为本地估算">估算</StatusTag>
              </td>
              <td class="num">{{ formatInt(r.outputTokens) }}</td>
              <td class="num">{{ formatInt(r.cachedTokens) }}</td>
              <td class="num">{{ formatLatency(r.latencyMs) }}</td>
              <td class="num">{{ formatCost(r.estimatedCost) }}</td>
              <td><StatusTag :tone="STATUS[r.status].tone">{{ STATUS[r.status].text }}</StatusTag></td>
            </template>
          </tr>
        </tbody>
      </table>
    </div>
    <footer v-if="!compact && rows.length > pageSize" class="pager">
      <span>共 {{ formatInt(rows.length) }} 条</span>
      <button class="btn" type="button" :disabled="page <= 1" aria-label="上一页" @click="page--"><ChevronLeft :size="16" /></button>
      <span data-test="page-indicator">{{ page }} / {{ pageCount }}</span>
      <button class="btn" type="button" :disabled="page >= pageCount" aria-label="下一页" @click="page++"><ChevronRight :size="16" /></button>
    </footer>
  </div>
</template>

<style scoped>
.scroll { overflow-x: auto; }
table { width: 100%; border-collapse: collapse; font-size: var(--fs-small); }
th { padding: 8px 10px; background: var(--c-sidebar); color: var(--c-text-2); font-weight: 500; font-size: var(--fs-caption); text-align: left; white-space: nowrap; }
td { padding: 8px 10px; border-bottom: 1px solid var(--c-border); white-space: nowrap; font-variant-numeric: tabular-nums; }
tbody tr:last-child td { border-bottom: 0; }
.num { text-align: right; }
.model { max-width: 190px; overflow: hidden; text-overflow: ellipsis; }
.mono { max-width: 150px; overflow: hidden; text-overflow: ellipsis; font-family: var(--font-mono); font-size: var(--fs-caption); }
.time { color: var(--c-text-2); }
.tok { display: flex; align-items: center; gap: 6px; }
.in { color: var(--c-brand); }
.out { color: var(--c-text-2); }
.compact td { padding: 7px 8px; }
.pager { display: flex; align-items: center; justify-content: flex-end; gap: 10px; padding: 10px 4px 0; color: var(--c-text-2); font-size: var(--fs-small); }
.pager .btn { width: 32px; padding: 0; }
</style>
