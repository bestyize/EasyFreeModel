<script setup lang="ts">
defineProps<{ modelValue: boolean; label: string; disabled?: boolean }>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()
</script>

<template>
  <button
    type="button"
    class="switch"
    role="switch"
    :aria-checked="modelValue"
    :aria-label="label"
    :title="label"
    :disabled="disabled"
    @click="emit('update:modelValue', !modelValue)"
  >
    <span class="knob" />
  </button>
</template>

<style scoped>
.switch { position: relative; flex: none; width: 36px; height: 20px; padding: 0; border: 0; border-radius: 10px; background: #cfd3db; transition: background 0.15s; }
.switch[aria-checked='true'] { background: var(--c-brand); }
.switch:disabled { opacity: 0.5; cursor: not-allowed; }
.knob { position: absolute; top: 2px; left: 2px; width: 16px; height: 16px; border-radius: 50%; background: #fff; transition: transform 0.15s; }
.switch[aria-checked='true'] .knob { transform: translateX(16px); }
</style>
