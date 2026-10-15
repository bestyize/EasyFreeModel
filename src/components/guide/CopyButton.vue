<script setup lang="ts">
import { onBeforeUnmount, ref } from 'vue'
import { Check, Copy } from 'lucide-vue-next'

const props = withDefaults(defineProps<{ text: string; label: string; showLabel?: boolean }>(), { showLabel: false })
const emit = defineEmits<{ error: [message: string] }>()

const copied = ref(false)
let timer: ReturnType<typeof setTimeout> | undefined

async function copy() {
  try {
    await navigator.clipboard.writeText(props.text)
    copied.value = true
    clearTimeout(timer)
    timer = setTimeout(() => (copied.value = false), 1500)
  } catch {
    copied.value = false
    emit('error', '复制失败，请手动选中文本后复制')
  }
}
onBeforeUnmount(() => clearTimeout(timer))
</script>

<template>
  <button class="copy" :class="{ labeled: showLabel }" type="button" :aria-label="copied ? '已复制' : label" :title="copied ? '已复制' : label" @click="copy">
    <component :is="copied ? Check : Copy" :size="14" aria-hidden="true" />
    <span v-if="showLabel">{{ copied ? '已复制' : label }}</span>
  </button>
</template>

<style scoped>
.copy { display: inline-flex; align-items: center; justify-content: center; gap: 6px; flex: none; height: 26px; min-width: 26px; padding: 0 6px; border: 1px solid var(--c-border); border-radius: 6px; background: var(--c-canvas); color: var(--c-text-2); font-size: var(--fs-caption); }
.copy:hover { background: var(--c-surface-2); color: var(--c-text); }
</style>
