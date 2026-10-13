<script setup lang="ts">
import StatusTag from '@/components/StatusTag.vue'
import type { MediaJob } from '@/types'
import { formatDateTime } from './format'
import { isActiveJob, JOB_STATUS } from './jobStatus'

defineProps<{ job: MediaJob | null; submitting: boolean; cancelling: boolean; cancelError: string | null }>()
const emit = defineEmits<{ cancel: [id: string] }>()
</script>

<template>
  <div class="panel">
    <h2 class="section-title">任务状态与结果</h2>
    <p v-if="submitting" class="muted" role="status">正在提交任务…</p>
    <p v-else-if="!job" class="muted" data-testid="no-job">尚无任务。提交后在此显示状态与结果。</p>
    <template v-else>
      <div class="meta">
        <StatusTag :tone="JOB_STATUS[job.status].tone" data-testid="job-status">{{ JOB_STATUS[job.status].label }}</StatusTag>
        <span class="sub">{{ job.providerId }} · {{ job.model || '默认模型' }} · {{ formatDateTime(job.createdAt) }}</span>
        <button
          v-if="isActiveJob(job.status)" class="btn cancel" type="button" data-testid="cancel-job"
          :disabled="cancelling" @click="emit('cancel', job.id)"
        >
          {{ cancelling ? '取消中…' : '取消任务' }}
        </button>
      </div>
      <p v-if="cancelError" class="err" role="alert">{{ cancelError }}</p>
      <p v-if="job.status === 'queued'" class="muted">任务已进入队列，等待处理。</p>
      <p v-else-if="job.status === 'running'" class="muted">处理中，状态会自动刷新。</p>
      <p v-else-if="job.status === 'cancelled'" class="muted">任务已取消。</p>
      <p v-else-if="job.status === 'failed'" class="err" role="alert" data-testid="job-error">失败：{{ job.error || '后端未返回错误原因' }}</p>
      <div v-else class="result"><slot :job="job" /></div>
    </template>
  </div>
</template>

<style scoped>
.panel { display: flex; flex-direction: column; gap: 12px; }
.meta { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.sub { color: var(--c-text-2); font-size: var(--fs-caption); }
.cancel { margin-left: auto; }
.muted { margin: 0; color: var(--c-text-2); font-size: var(--fs-small); }
.err { margin: 0; color: var(--c-danger); font-size: var(--fs-small); }
</style>
