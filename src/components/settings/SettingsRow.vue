<script setup lang="ts">
defineProps<{ label: string; description?: string; error?: string | null; forId?: string }>()
</script>

<template>
  <div class="row">
    <div class="text">
      <label :for="forId" class="label">{{ label }}</label>
      <p v-if="description" class="desc">{{ description }}</p>
      <p v-if="error" class="err" role="alert">{{ error }}</p>
      <slot name="extra" />
    </div>
    <div class="control"><slot /></div>
  </div>
</template>

<style scoped>
.row { display: flex; align-items: flex-start; justify-content: space-between; gap: 24px; padding: 14px 0; border-top: 1px solid var(--c-border); }
.row:first-of-type { border-top: 0; }
.text { flex: 1; min-width: 0; }
.label { font-weight: 500; font-size: var(--fs-body); }
.desc { margin: 2px 0 0; color: var(--c-text-2); font-size: var(--fs-caption); line-height: 1.5; }
.err { margin: 4px 0 0; color: var(--c-danger); font-size: var(--fs-caption); }
.control { flex: none; display: flex; align-items: center; gap: 8px; min-height: var(--control-h); }
</style>
