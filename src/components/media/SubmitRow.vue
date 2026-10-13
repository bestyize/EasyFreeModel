<script setup lang="ts">
defineProps<{ error: string | null }>()
</script>

<template>
  <div class="submit">
    <div class="buttons"><slot /></div>
    <p v-if="error" class="err" role="alert" data-testid="submit-error">提交失败：{{ error }}</p>
  </div>
</template>

<style scoped>
.submit { display: flex; flex-direction: column; gap: 8px; }
.buttons { display: flex; gap: 8px; }
.err { margin: 0; padding: 8px 10px; border-radius: 8px; background: #fdecec; color: var(--c-danger); font-size: var(--fs-small); overflow-wrap: anywhere; }
</style>
