<script setup lang="ts">
defineProps<{ modes: { id: string; label: string }[]; modelValue: string; disabled?: boolean }>()
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()
</script>

<template>
  <div class="tabs" role="tablist">
    <button
      v-for="m in modes" :key="m.id" type="button" role="tab" class="tab" :class="{ active: m.id === modelValue }"
      :aria-selected="m.id === modelValue" :disabled="disabled" :data-testid="`tab-${m.id}`" @click="emit('update:modelValue', m.id)"
    >
      {{ m.label }}
    </button>
  </div>
</template>

<style scoped>
.tabs { display: inline-flex; padding: 3px; border-radius: 8px; background: var(--c-surface-2); align-self: flex-start; }
.tab { height: 30px; padding: 0 16px; border: 0; border-radius: 6px; background: transparent; color: var(--c-text-2); font-size: var(--fs-small); }
.tab.active { background: var(--c-canvas); color: var(--c-brand); font-weight: 600; }
.tab:disabled { cursor: not-allowed; opacity: 0.6; }
</style>
