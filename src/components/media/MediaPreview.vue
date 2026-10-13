<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { convertFileSrc } from '@tauri-apps/api/core'

const props = defineProps<{ kind: 'image' | 'audio' | 'video'; path: string }>()

const inTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
const failed = ref(false)
watch(() => props.path, () => (failed.value = false))

/** 本地文件需经 Tauri asset 协议访问；浏览器预览无法读取本地路径，不伪造预览。 */
const src = computed(() => (inTauri ? convertFileSrc(props.path) : null))
</script>

<template>
  <div class="preview">
    <template v-if="src && !failed">
      <img v-if="kind === 'image'" :src="src" alt="生成结果预览" @error="failed = true" />
      <audio v-else-if="kind === 'audio'" :src="src" controls @error="failed = true" />
      <video v-else :src="src" controls @error="failed = true" />
    </template>
    <p v-else class="note" data-testid="preview-fallback">
      {{ inTauri ? '无法加载预览（可能未启用 asset 协议或文件已被移动）。' : '浏览器预览环境无法读取本地文件。' }}
    </p>
    <p class="path">文件位置：<code>{{ path }}</code></p>
  </div>
</template>

<style scoped>
.preview { display: flex; flex-direction: column; gap: 8px; }
img, video { max-width: 100%; max-height: 420px; border-radius: 8px; border: 1px solid var(--c-border); object-fit: contain; background: var(--c-sidebar); }
audio { width: 100%; }
.note { margin: 0; color: var(--c-text-2); font-size: var(--fs-small); }
.path { margin: 0; font-size: var(--fs-caption); color: var(--c-text-2); word-break: break-all; }
</style>
