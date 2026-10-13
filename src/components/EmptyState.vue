<script setup lang="ts">
defineProps<{ title: string; description?: string; dashed?: boolean }>()
</script>

<template>
  <div class="empty-state" :class="{ dashed }">
    <p class="title">{{ title }}</p>
    <p v-if="description" class="desc">{{ description }}</p>
    <div v-if="$slots.default" class="actions"><slot /></div>
  </div>
</template>

<style scoped>
.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 6px;
  padding: 32px 16px;
  border-radius: var(--radius);
  text-align: center;
}
.dashed {
  border: 1px dashed var(--c-border);
}
.title { margin: 0; font-weight: 600; }
.desc { margin: 0; color: var(--c-text-2); font-size: var(--fs-small); }
.actions { margin-top: 8px; display: flex; gap: 8px; }
</style>
