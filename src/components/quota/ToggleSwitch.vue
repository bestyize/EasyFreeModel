<script setup lang="ts">
const props = defineProps<{ modelValue: boolean; label: string; disabled?: boolean }>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean]; change: [value: boolean] }>()

function toggle() {
  const next = !props.modelValue
  emit('update:modelValue', next)
  emit('change', next)
}
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
    @click="toggle"
  >
    <span class="knob" />
  </button>
</template>

<style scoped>
.switch { position: relative; flex: none; width: 38px; height: 22px; padding: 0; border: 0; border-radius: 11px; background: #cfd3da; transition: background 0.15s; }
.switch.on { background: var(--c-brand); }
.switch:disabled { opacity: 0.5; cursor: not-allowed; }
.knob { position: absolute; top: 2px; left: 2px; width: 18px; height: 18px; border-radius: 50%; background: #fff; transition: transform 0.15s; }
.on .knob { transform: translateX(16px); }
</style>
