<script setup lang="ts" generic="T extends string">
defineProps<{ modelValue: T; options: { id: T; label: string }[]; label: string }>()
const emit = defineEmits<{ 'update:modelValue': [value: T] }>()
</script>

<template>
  <div class="tabs" role="tablist" :aria-label="label">
    <button
      v-for="o in options" :key="o.id" type="button" role="tab" class="tab"
      :class="{ active: o.id === modelValue }" :aria-selected="o.id === modelValue" :data-testid="`tab-${o.id}`"
      @click="emit('update:modelValue', o.id)"
    >
      {{ o.label }}
    </button>
  </div>
</template>

<style scoped>
.tabs { display: inline-flex; gap: 2px; padding: 3px; border-radius: 9px; background: var(--c-surface-2); }
.tab { height: 28px; padding: 0 12px; border: 0; border-radius: 7px; background: transparent; color: var(--c-text-2); font-size: var(--fs-small); }
.tab:hover { color: var(--c-text); }
.tab.active { background: var(--c-canvas); color: var(--c-brand); font-weight: 600; box-shadow: 0 1px 2px rgba(22, 24, 29, 0.08); }
</style>
