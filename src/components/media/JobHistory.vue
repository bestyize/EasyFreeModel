<script setup lang="ts">
import { RefreshCw } from 'lucide-vue-next'
import EmptyState from '@/components/EmptyState.vue'
import StatusTag from '@/components/StatusTag.vue'
import type { MediaJob } from '@/types'
import { formatDateTime } from './format'
import { isActiveJob, JOB_STATUS } from './jobStatus'

defineProps<{
  jobs: MediaJob[]
  loading: boolean
  error: string | null
  cancelling: string[]
  /** 摘要是否是大段结构化文本（如搜索 JSON），为 true 时历史里不展开 */
  hideSummary?: boolean
}>()
const emit = defineEmits<{ refresh: []; cancel: [id: string] }>()
</script>

<template>
  <div class="history">
    <div class="head">
      <h2 class="section-title">历史</h2>
      <button class="btn" type="button" :disabled="loading" @click="emit('refresh')">
        <RefreshCw :size="14" aria-hidden="true" /> {{ loading ? '刷新中…' : '刷新' }}
      </button>
    </div>
    <p v-if="error" class="err" role="alert">加载历史失败：{{ error }}</p>
    <EmptyState v-if="!error && jobs.length === 0 && !loading" title="暂无历史记录" description="提交任务后会在这里留下记录。" />
    <table v-else-if="jobs.length > 0">
      <thead>
        <tr><th>时间</th><th>供应商 / 模型</th><th v-if="!hideSummary">摘要</th><th>状态</th><th>结果</th><th>操作</th></tr>
      </thead>
      <tbody>
        <tr v-for="j in jobs" :key="j.id" :data-testid="`job-${j.id}`">
          <td>{{ formatDateTime(j.createdAt) }}</td>
          <td>{{ j.providerId }}<div class="sub">{{ j.model || '默认模型' }}</div></td>
          <td v-if="!hideSummary" class="summary">{{ j.summary }}</td>
          <td><StatusTag :tone="JOB_STATUS[j.status].tone">{{ JOB_STATUS[j.status].label }}</StatusTag></td>
          <td class="result">
            <span v-if="j.error" class="err">{{ j.error }}</span>
            <code v-else-if="j.outputPath">{{ j.outputPath }}</code>
            <span v-else class="sub">—</span>
          </td>
          <td>
            <button
              v-if="isActiveJob(j.status)" class="btn" type="button" :disabled="cancelling.includes(j.id)"
              @click="emit('cancel', j.id)"
            >
              {{ cancelling.includes(j.id) ? '取消中…' : '取消' }}
            </button>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>

<style scoped>
.head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 12px; }
table { width: 100%; border-collapse: collapse; font-size: var(--fs-small); }
th { padding: 8px 10px; text-align: left; color: var(--c-text-2); font-weight: 500; background: var(--c-sidebar); white-space: nowrap; }
td { padding: 10px; border-top: 1px solid var(--c-border); vertical-align: top; }
.sub { color: var(--c-text-2); font-size: var(--fs-caption); }
.summary { max-width: 260px; overflow-wrap: anywhere; }
.result { max-width: 280px; overflow-wrap: anywhere; }
.err { margin: 0 0 8px; color: var(--c-danger); font-size: var(--fs-small); }
code { font-family: var(--font-mono); font-size: var(--fs-caption); }
</style>
