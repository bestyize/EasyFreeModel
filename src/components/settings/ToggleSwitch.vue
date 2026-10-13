<script setup lang="ts">
defineProps<{ modelValue: boolean; disabled?: boolean; label: string }>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()
</script>

<template>
  <button
    type="button"
    role="switch"
    class="switch"
    :class="{ on: modelValue }"
    :aria-checked="modelValue"
    :aria-label="label"
    :disabled="disabled"
    @click="emit('update:modelValue', !modelValue)"
  >
    <span class="knob" />
  </button>
</template>

<style scoped>
.switch { position: relative; flex: none; width: 38px; height: 22px; padding: 0; border: 0; border-radius: 11px; background: #c9ced8; transition: background 0.15s; }
.switch.on { background: var(--c-brand); }
.switch:disabled { opacity: 0.5; cursor: not-allowed; }
.knob { position: absolute; top: 2px; left: 2px; width: 18px; height: 18px; border-radius: 50%; background: #fff; transition: transform 0.15s; }
.switch.on .knob { transform: translateX(16px); }
</style>
