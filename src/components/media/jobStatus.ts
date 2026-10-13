import type { JobStatus } from '@/types'
import type { StatusTone } from '@/components/StatusTag.vue'

export const JOB_STATUS: Record<JobStatus, { label: string; tone: StatusTone }> = {
  queued: { label: '排队中', tone: 'neutral' },
  running: { label: '处理中', tone: 'warning' },
  succeeded: { label: '成功', tone: 'success' },
  failed: { label: '失败', tone: 'danger' },
  cancelled: { label: '已取消', tone: 'neutral' },
}

export function isActiveJob(status: JobStatus): boolean {
  return status === 'queued' || status === 'running'
}
