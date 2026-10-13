<script setup lang="ts">
import { computed } from 'vue'
import StatusTag from '@/components/StatusTag.vue'
import { parseSearchPayload } from './searchResults'

const props = defineProps<{ summary: string }>()
const payload = computed(() => parseSearchPayload(props.summary))
</script>

<template>
  <div class="search">
    <template v-if="payload">
      <div class="origin">
        <StatusTag v-if="payload.origin === 'native'" tone="success" data-testid="origin-tag">原生搜索接口</StatusTag>
        <StatusTag v-else-if="payload.origin === 'aggregated'" tone="warning" data-testid="origin-tag">聚合结果（非上游原生）</StatusTag>
        <StatusTag v-else tone="neutral" data-testid="origin-tag">来源未标明</StatusTag>
        <span v-if="payload.provider" class="prov">{{ payload.provider }}</span>
      </div>
      <p v-if="payload.results.length === 0" class="empty">未返回有效结果。</p>
      <ol v-else>
        <li v-for="(r, i) in payload.results" :key="i">
          <a :href="r.url" target="_blank" rel="noopener noreferrer">{{ r.title }}</a>
          <div class="url">{{ r.url }}</div>
          <p v-if="r.snippet" class="snippet">{{ r.snippet }}</p>
        </li>
      </ol>
    </template>
    <template v-else>
      <p class="empty">无法解析结构化搜索结果，以下为后端返回的原始文本：</p>
      <pre>{{ summary }}</pre>
    </template>
  </div>
</template>

<style scoped>
.origin { display: flex; align-items: center; gap: 8px; margin-bottom: 10px; }
.prov { color: var(--c-text-2); font-size: var(--fs-caption); }
ol { margin: 0; padding-left: 20px; display: flex; flex-direction: column; gap: 12px; }
a { color: var(--c-brand); font-weight: 500; text-decoration: none; }
a:hover { text-decoration: underline; }
.url { color: var(--c-success); font-size: var(--fs-caption); word-break: break-all; }
.snippet { margin: 2px 0 0; color: var(--c-text-2); font-size: var(--fs-small); line-height: 1.5; }
.empty { margin: 0 0 8px; color: var(--c-text-2); font-size: var(--fs-small); }
pre { margin: 0; padding: 10px; border-radius: 8px; background: var(--c-surface-2); white-space: pre-wrap; word-break: break-word; font: var(--fs-caption) var(--font-mono); }
</style>
