<script setup lang="ts">
import { ref } from 'vue'
import CopyButton from './CopyButton.vue'

defineProps<{ text: string; title?: string }>()
const error = ref<string | null>(null)
</script>

<template>
  <figure class="block">
    <figcaption v-if="title" class="bar"><span class="title">{{ title }}</span></figcaption>
    <div class="body">
      <pre><code>{{ text }}</code></pre>
      <CopyButton class="float" :text="text" label="复制" show-label @error="error = $event" />
    </div>
    <p v-if="error" class="err" role="alert">{{ error }}</p>
  </figure>
</template>

<style scoped>
.block { margin: 0; border: 1px solid var(--c-border); border-radius: 8px; background: var(--c-sidebar); overflow: hidden; }
.bar { padding: 6px 12px; border-bottom: 1px solid var(--c-border); color: var(--c-text-2); font-size: var(--fs-caption); }
.title { font-family: var(--font-mono); }
.body { position: relative; }
pre { margin: 0; padding: 12px 84px 12px 14px; overflow-x: auto; font-family: var(--font-mono); font-size: var(--fs-small); line-height: 1.65; }
code { font-family: inherit; white-space: pre; }
.float { position: absolute; top: 8px; right: 8px; }
.err { margin: 0; padding: 6px 12px; color: var(--c-danger); font-size: var(--fs-caption); }
</style>
