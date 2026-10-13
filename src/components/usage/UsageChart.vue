<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import type { UsageSummary } from '@/types'
import EmptyState from '@/components/EmptyState.vue'
import { buildTrendOption, seriesFor, TREND_METRICS, type TrendMetric } from './chartOptions'
import { useECharts } from './useECharts'

const props = defineProps<{ trend: UsageSummary['trend'] }>()

const metric = ref<TrendMetric>('token')
/** 被用户点击图例隐藏的系列名 */
const hidden = ref<string[]>([])
const el = ref<HTMLElement | null>(null)

const legend = computed(() => seriesFor(metric.value))
const hasData = computed(() => props.trend.length > 0)

watch(metric, () => (hidden.value = []))

function toggle(name: string) {
  hidden.value = hidden.value.includes(name) ? hidden.value.filter((n) => n !== name) : [...hidden.value, name]
}

useECharts(
  el,
  () => buildTrendOption(props.trend, metric.value, hidden.value),
  () => metric.value,
)
</script>

<template>
  <section class="card chart-card" aria-label="使用趋势">
    <header>
      <h2 class="section-title">使用趋势</h2>
      <ul class="legend" aria-label="图例（点击显示/隐藏）">
        <li v-for="s in legend" :key="s.name">
          <button
            type="button"
            class="legend-item"
            :class="{ off: hidden.includes(s.name) }"
            :aria-pressed="!hidden.includes(s.name)"
            @click="toggle(s.name)"
          >
            <i :style="{ background: s.color }" />{{ s.name }}
          </button>
        </li>
      </ul>
    </header>
    <div class="seg" role="tablist" aria-label="趋势指标">
      <button
        v-for="m in TREND_METRICS"
        :key="m.value"
        type="button"
        role="tab"
        :aria-selected="metric === m.value"
        :class="{ active: metric === m.value }"
        @click="metric = m.value"
      >
        {{ m.label }}
      </button>
    </div>
    <div v-show="hasData" ref="el" class="canvas" role="img" aria-label="使用趋势折线图" />
    <EmptyState v-if="!hasData" title="该时间范围暂无趋势数据" class="empty" />
  </section>
</template>

<style scoped>
.chart-card { padding: 16px 18px; min-width: 0; }
header { display: flex; align-items: center; justify-content: space-between; gap: 12px; flex-wrap: wrap; }
.legend { display: flex; gap: 6px; margin: 0; padding: 0; list-style: none; }
.legend-item { display: inline-flex; align-items: center; gap: 6px; height: 26px; padding: 0 8px; border: 0; border-radius: 6px; background: transparent; color: var(--c-text-2); font-size: var(--fs-caption); }
.legend-item:hover { background: var(--c-surface-2); }
.legend-item i { width: 8px; height: 8px; border-radius: 50%; }
.legend-item.off { opacity: 0.45; text-decoration: line-through; }
.seg { display: inline-flex; margin-top: 10px; padding: 3px; border: 1px solid var(--c-border); border-radius: 8px; background: var(--c-sidebar); }
.seg button { height: 28px; padding: 0 16px; border: 0; border-radius: 6px; background: transparent; color: var(--c-text-2); font-size: var(--fs-small); }
.seg button.active { background: var(--c-canvas); color: var(--c-brand); box-shadow: 0 0 0 1px var(--c-brand-soft); }
.canvas { width: 100%; height: 280px; margin-top: 8px; }
.empty { height: 280px; }
</style>
