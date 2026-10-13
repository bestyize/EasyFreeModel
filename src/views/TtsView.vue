<script setup lang="ts">
import { computed } from 'vue'
import StatusTag from '@/components/StatusTag.vue'
import DynamicForm from '@/components/media/DynamicForm.vue'
import ProviderPicker from '@/components/media/ProviderPicker.vue'
import SubmitRow from '@/components/media/SubmitRow.vue'
import WorkbenchLayout from '@/components/media/WorkbenchLayout.vue'
import { MEDIA_CAPABILITIES, TTS_FORMATS, TTS_MAX_CHARS } from '@/components/media/capabilities'
import { useMediaPage } from '@/components/media/useMediaPage'

const fields = computed(() => MEDIA_CAPABILITIES.tts.modes[0].fields)
const { state, busy, providers, providerId, model, values, errors, setValue, submit } = useMediaPage('tts', fields)

/** 试听与保存都是提交合成任务，由 intent 告知后端是否需要落盘保存 */
function run(intent: 'preview' | 'save') {
  void submit({ engine: 'cloud', intent })
}
</script>

<template>
  <WorkbenchLayout kind="tts">
    <template #tabs>
      <div class="engine" role="group" aria-label="合成方式">
        <span class="seg active"><StatusTag tone="brand" :dot="false">云端 TTS</StatusTag></span>
        <span class="seg disabled" title="本地 TTS 为后期扩展，尚未实现"><StatusTag tone="neutral" :dot="false">本地 TTS · 待实现</StatusTag></span>
      </div>
    </template>
    <template #form>
      <form class="form" novalidate @submit.prevent="run('save')">
        <ProviderPicker v-model:provider-id="providerId" v-model:model="model" :providers="providers" :disabled="busy" />
        <p class="limits" data-testid="tts-limits">
          云端合成 · 最大 {{ TTS_MAX_CHARS }} 字 · 格式 {{ TTS_FORMATS.map((f) => f.toUpperCase()).join(' / ') }}。文本会发送到所选云端供应商。
        </p>
        <DynamicForm :fields="fields" :values="values" :errors="errors" :provider-id="providerId" :disabled="busy" @change="setValue" />
        <SubmitRow :error="state.submitError">
          <button class="btn" type="button" data-testid="preview" :disabled="busy" @click="run('preview')">试听</button>
          <button class="btn btn-primary" type="submit" data-testid="submit" :disabled="busy">
            {{ busy ? '提交中…' : '合成并保存' }}
          </button>
        </SubmitRow>
      </form>
    </template>
  </WorkbenchLayout>
</template>

<style scoped>
.form { display: flex; flex-direction: column; gap: 16px; }
.engine { display: inline-flex; gap: 8px; align-self: flex-start; }
.seg.disabled { opacity: 0.7; cursor: not-allowed; }
.limits { margin: 0; color: var(--c-text-2); font-size: var(--fs-caption); line-height: 1.5; }
</style>
