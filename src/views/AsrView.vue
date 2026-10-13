<script setup lang="ts">
import { computed, ref } from 'vue'
import { Mic, Square } from 'lucide-vue-next'
import AudioDropZone from '@/components/media/AudioDropZone.vue'
import DynamicForm from '@/components/media/DynamicForm.vue'
import ProviderPicker from '@/components/media/ProviderPicker.vue'
import SubmitRow from '@/components/media/SubmitRow.vue'
import WorkbenchLayout from '@/components/media/WorkbenchLayout.vue'
import { AUDIO_FILE, MEDIA_CAPABILITIES } from '@/components/media/capabilities'
import { useAudioRecorder } from '@/components/media/useAudioRecorder'
import { useMediaPage } from '@/components/media/useMediaPage'

const fields = computed(() => MEDIA_CAPABILITIES.asr.modes[0].fields)
const { state, busy, providers, providerId, model, values, errors, setValue, submit } = useMediaPage('asr', fields)

const ack = ref(false)
const ackError = ref<string | null>(null)
const audio = computed(() => (values.audio instanceof File ? values.audio : null))

const recorder = useAudioRecorder({
  maxSeconds: AUDIO_FILE.maxRecordSeconds,
  onFinish: (file) => setValue('audio', file),
})

function mmss(s: number): string {
  return `${String(Math.floor(s / 60)).padStart(2, '0')}:${String(s % 60).padStart(2, '0')}`
}

function checkAck(): boolean {
  ackError.value = ack.value ? null : '请先确认已了解音频将上传到所选供应商'
  return ack.value
}

function onSubmit() {
  void submit({}, checkAck)
}

const copied = ref(false)
async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text)
    copied.value = true
  } catch {
    copied.value = false
  }
}
</script>

<template>
  <WorkbenchLayout kind="asr">
    <template #form>
      <form class="form" novalidate @submit.prevent="onSubmit">
        <ProviderPicker v-model:provider-id="providerId" v-model:model="model" :providers="providers" :disabled="busy" />

        <AudioDropZone
          :file="audio" :error="errors.audio ?? null" :accept="AUDIO_FILE.accept" :max-bytes="AUDIO_FILE.maxBytes"
          :disabled="busy || recorder.recording.value" @select="setValue('audio', $event)"
        />

        <div class="record">
          <button v-if="!recorder.recording.value" class="btn" type="button" data-testid="record" :disabled="busy" @click="recorder.start()">
            <Mic :size="14" aria-hidden="true" /> 录音
          </button>
          <button v-else class="btn btn-danger" type="button" data-testid="stop-record" @click="recorder.stop()">
            <Square :size="14" aria-hidden="true" /> 停止录音 {{ mmss(recorder.seconds.value) }}
          </button>
          <span class="hint">使用系统麦克风，首次使用需授权；最长 {{ AUDIO_FILE.maxRecordSeconds / 60 }} 分钟。</span>
        </div>
        <p v-if="recorder.error.value" class="err" role="alert" data-testid="record-error">{{ recorder.error.value }}</p>

        <DynamicForm
          :fields="fields" :values="values" :errors="errors" :provider-id="providerId" :skip-keys="['audio']"
          :disabled="busy" @change="setValue"
        />

        <div class="privacy">
          <label class="ack">
            <input v-model="ack" type="checkbox" data-testid="ack" :disabled="busy" @change="checkAck" />
            <span>我了解：音频内容会被上传到所选供应商进行转写，其中可能包含个人或敏感信息。</span>
          </label>
          <p v-if="ackError" class="err" role="alert">{{ ackError }}</p>
        </div>

        <SubmitRow :error="state.submitError">
          <button class="btn btn-primary" type="submit" data-testid="submit" :disabled="busy || recorder.recording.value">
            {{ busy ? '转写中…' : '开始转写' }}
          </button>
        </SubmitRow>
      </form>
    </template>

    <template #result="{ job }">
      <div class="transcript">
        <pre data-testid="transcript">{{ job.summary || '（供应商未返回文本）' }}</pre>
        <div class="row">
          <button class="btn" type="button" :disabled="!job.summary" @click="copyText(job.summary)">{{ copied ? '已复制' : '复制文本' }}</button>
          <code v-if="job.outputPath" class="path">{{ job.outputPath }}</code>
        </div>
      </div>
    </template>
  </WorkbenchLayout>
</template>

<style scoped>
.form { display: flex; flex-direction: column; gap: 16px; }
.record { display: flex; align-items: center; gap: 12px; }
.hint { color: var(--c-text-2); font-size: var(--fs-caption); }
.err { margin: 0; color: var(--c-danger); font-size: var(--fs-caption); }
.ack { display: flex; align-items: flex-start; gap: 8px; font-size: var(--fs-small); line-height: 1.5; }
.ack input { margin-top: 3px; }
.transcript pre { margin: 0 0 10px; padding: 12px; border-radius: 8px; background: var(--c-surface-2); white-space: pre-wrap; overflow-wrap: anywhere; font: var(--fs-small) var(--font); max-height: 360px; overflow-y: auto; }
.row { display: flex; align-items: center; gap: 12px; }
.path { font-size: var(--fs-caption); color: var(--c-text-2); overflow-wrap: anywhere; }
</style>
