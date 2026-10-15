<script setup lang="ts">
export type StatusTone = 'success' | 'warning' | 'danger' | 'neutral' | 'brand'

withDefaults(defineProps<{ tone?: StatusTone; dot?: boolean }>(), { tone: 'neutral', dot: true })
</script>

<template>
  <span class="status-tag" :class="`tone-${tone}`">
    <i v-if="dot" class="dot" aria-hidden="true" />
    <slot />
  </span>
</template>

<style scoped>
.status-tag {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 22px;
  padding: 0 8px;
  border-radius: 11px;
  font-size: var(--fs-caption);
  line-height: 1;
  white-space: nowrap;
}
.dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: currentColor;
}
.tone-success { color: var(--c-success); background: var(--c-success-soft); }
.tone-warning { color: var(--c-warning); background: var(--c-warning-soft); }
.tone-danger { color: var(--c-danger); background: var(--c-danger-soft); }
.tone-brand { color: var(--c-brand); background: var(--c-brand-soft); }
.tone-neutral { color: var(--c-text-2); background: var(--c-surface-2); }
</style>
