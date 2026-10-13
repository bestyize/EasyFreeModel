<script setup lang="ts">
import { computed } from 'vue'
import EmptyState from '@/components/EmptyState.vue'
import { useMediaStore } from '@/stores/media'
import type { MediaJob, MediaKind } from '@/types'
import { MEDIA_CAPABILITIES } from './capabilities'
import JobHistory from './JobHistory.vue'
import JobStatusPanel from './JobStatusPanel.vue'
import MediaPreview from './MediaPreview.vue'

const props = defineProps<{ kind: MediaKind; hideHistorySummary?: boolean }>()
defineSlots<{
  form(): unknown
  /** 覆盖默认结果预览（默认：有输出路径的图片 / 音频 / 视频预览，否则显示摘要） */
  result?(props: { job: MediaJob }): unknown
  tabs?(): unknown
}>()

const media = useMediaStore()
const cap = computed(() => MEDIA_CAPABILITIES[props.kind])
const state = computed(() => media.kinds[props.kind])
const job = computed(() => media.currentJob(props.kind))
const gate = computed<'loading' | 'error' | 'empty' | 'ready'>(() => {
  if (!media.providersLoaded) return media.providersError ? 'error' : 'loading'
  return media.availableProviders.length === 0 ? 'empty' : 'ready'
})
const previewKind = computed(() => (['image', 'audio', 'video'] as const).find((k) => (props.kind === 'tts' ? 'audio' : props.kind) === k))
</script>

<template>
  <div class="workbench">
    <slot name="tabs" />

    <p v-if="gate === 'loading'" class="muted">正在检查可用供应商…</p>
    <div v-else-if="gate === 'error'" class="card err-card" role="alert">
      <span>无法获取供应商列表：{{ media.providersError }}</span>
      <button class="btn" type="button" @click="media.loadProviders()">重试</button>
    </div>
    <EmptyState
      v-else-if="gate === 'empty'"
      title="暂无可用供应商"
      :description="`${cap.unavailableReason}待适配器接入并在「提供商」页启用后，这里会出现可用的供应商。`"
      dashed
    />
    <div v-else class="top">
      <section class="card pane" aria-label="参数"><slot name="form" /></section>
      <section class="card pane" aria-label="任务状态与结果">
        <JobStatusPanel
          :job="job"
          :submitting="state.submitting"
          :cancelling="!!job && state.cancelling.includes(job.id)"
          :cancel-error="state.cancelError"
          @cancel="media.cancel(kind, $event)"
        >
          <template #default="{ job: j }">
            <slot name="result" :job="j">
              <MediaPreview v-if="previewKind && j.outputPath" :kind="previewKind" :path="j.outputPath" />
              <p v-else class="summary">{{ j.summary || '任务已完成。' }}</p>
            </slot>
          </template>
        </JobStatusPanel>
      </section>
    </div>

    <section class="card pane" aria-label="历史">
      <JobHistory
        :jobs="state.jobs"
        :loading="state.loading"
        :error="state.loadError"
        :cancelling="state.cancelling"
        :hide-summary="hideHistorySummary"
        @refresh="media.loadJobs(kind)"
        @cancel="media.cancel(kind, $event)"
      />
    </section>
  </div>
</template>

<style scoped>
.workbench { display: flex; flex-direction: column; gap: 16px; }
.top { display: grid; grid-template-columns: minmax(360px, 5fr) minmax(320px, 6fr); gap: 16px; align-items: start; }
.pane { padding: 18px 20px; min-width: 0; }
.muted { margin: 0; color: var(--c-text-2); }
.err-card { display: flex; align-items: center; justify-content: space-between; padding: 14px 16px; color: var(--c-danger); }
.summary { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; }
</style>
