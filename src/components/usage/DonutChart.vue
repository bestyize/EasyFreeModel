<script setup lang="ts">
import { computed, ref } from 'vue'
import type { UsageSummary } from '@/types'
import EmptyState from '@/components/EmptyState.vue'
import { buildDonutOption } from './chartOptions'
import { colorFor, formatCompact, formatPct } from './format'
import { useECharts } from './useECharts'

const props = defineProps<{ items: UsageSummary['providerShare'] }>()

const el = ref<HTMLElement | null>(null)
const total = computed(() => props.items.reduce((sum, i) => sum + i.tokens, 0))

useECharts(el, () => buildDonutOption(props.items))
</script>

<template>
  <section class="card donut-card" aria-label="提供商流量占比">
    <h2 class="section-title">提供商流量占比</h2>
    <div v-if="items.length" class="body">
      <div class="chart-wrap">
        <div ref="el" class="chart" role="img" aria-label="提供商流量占比圆环图" />
        <div class="center">
          <span class="cap">总 Token</span>
          <b>{{ formatCompact(total) }}</b>
        </div>
      </div>
      <ul class="rows">
        <li v-for="i in items" :key="i.providerId">
          <i class="dot" :style="{ background: colorFor(i.providerId) }" />
          <span class="name">{{ i.name }}</span>
          <span class="pct">{{ formatPct(i.pct) }}</span>
          <span class="tok">{{ formatCompact(i.tokens) }}</span>
        </li>
      </ul>
    </div>
    <EmptyState v-else title="暂无提供商数据" />
  </section>
</template>

<style scoped>
.donut-card { padding: 16px 18px; min-width: 0; }
.body { display: flex; align-items: center; gap: 20px; margin-top: 12px; flex-wrap: wrap; }
.chart-wrap { position: relative; flex: none; width: 168px; height: 168px; }
.chart { width: 100%; height: 100%; }
.center { position: absolute; inset: 0; display: grid; place-content: center; text-align: center; pointer-events: none; }
.cap { color: var(--c-text-2); font-size: var(--fs-caption); }
.center b { font-size: 20px; font-weight: 600; }
.rows { flex: 1; min-width: 180px; margin: 0; padding: 0; list-style: none; display: grid; gap: 10px; }
.rows li { display: grid; grid-template-columns: 10px minmax(0, 1fr) 52px 52px; align-items: center; gap: 8px; font-size: var(--fs-small); font-variant-numeric: tabular-nums; }
.dot { width: 8px; height: 8px; border-radius: 50%; }
.name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.pct { text-align: right; }
.tok { text-align: right; color: var(--c-text-2); }
</style>
