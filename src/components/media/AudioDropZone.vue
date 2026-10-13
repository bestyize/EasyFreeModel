<script setup lang="ts">
import { ref } from 'vue'
import { FileAudio } from 'lucide-vue-next'
import { formatBytes } from './format'

defineProps<{ file: File | null; error: string | null; accept: string[]; maxBytes: number; disabled?: boolean }>()
const emit = defineEmits<{ select: [file: File | null] }>()

const over = ref(false)
const input = ref<HTMLInputElement | null>(null)

function pick(files: FileList | null | undefined) {
  const f = files?.[0]
  if (f) emit('select', f)
}
function onDrop(e: DragEvent) {
  over.value = false
  pick(e.dataTransfer?.files)
}
</script>

<template>
  <div>
    <div
      class="zone" :class="{ over, invalid: !!error }" data-testid="dropzone"
      @dragover.prevent="!disabled && (over = true)" @dragleave="over = false" @drop.prevent="!disabled && onDrop($event)"
    >
      <FileAudio :size="28" aria-hidden="true" />
      <template v-if="file">
        <p class="name" data-testid="file-name">{{ file.name }}（{{ formatBytes(file.size) }}）</p>
        <button class="btn" type="button" :disabled="disabled" @click="emit('select', null)">移除</button>
      </template>
      <template v-else>
        <p class="name">拖入音频文件到此处，或</p>
        <button class="btn" type="button" :disabled="disabled" @click="input?.click()">选择文件</button>
      </template>
      <p class="hint">支持 {{ accept.join(' / ') }}，最大 {{ formatBytes(maxBytes) }}。</p>
      <input
        ref="input" class="hidden" type="file" :accept="accept.map((e) => '.' + e).join(',')" tabindex="-1"
        data-testid="file-input" @change="pick(($event.target as HTMLInputElement).files); ($event.target as HTMLInputElement).value = ''"
      />
    </div>
    <p v-if="error" class="err" role="alert">{{ error }}</p>
  </div>
</template>

<style scoped>
.zone { display: flex; flex-direction: column; align-items: center; gap: 8px; padding: 22px 16px; border: 1px dashed var(--c-border); border-radius: var(--radius); color: var(--c-text-2); text-align: center; }
.zone.over { border-color: var(--c-brand); background: var(--c-brand-soft); }
.zone.invalid { border-color: var(--c-danger); }
.name { margin: 0; color: var(--c-text); font-size: var(--fs-small); overflow-wrap: anywhere; }
.hint { margin: 0; font-size: var(--fs-caption); }
.err { margin: 6px 0 0; color: var(--c-danger); font-size: var(--fs-caption); }
.hidden { display: none; }
</style>
