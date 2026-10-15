<script setup lang="ts">
import { computed } from 'vue'
import type { QuotaPeriod } from '@/types'
import StatusTag from '@/components/StatusTag.vue'
import QuotaBar from './QuotaBar.vue'
import {
  DEFAULT_THRESHOLDS,
  formatCountdown,
  formatRemainingPct,
  quotaLevel,
  remainingPct,
  sourceLabel,
  usageText,
  type QuotaThresholds,
} from './quota'

const props = withDefaults(
  defineProps<{ period: QuotaPeriod; now: number; thresholds?: QuotaThresholds }>(),
  { thresholds: () => DEFAULT_THRESHOLDS },
)

const pct = computed(() => remainingPct(props.period))
const level = computed(() => quotaLevel(pct.value, props.thresholds))
const source = computed(() => sourceLabel(props.period.source))
const reset = computed(() => formatCountdown(props.period.resetsAt, props.now))
</script>

<template>
  <div class="period" :class="`lv-${level}`" data-test="period">
    <div class="top">
      <i class="dot" aria-hidden="true" />
      <span class="name" :title="period.name">{{ period.name }}</span>
      <StatusTag v-if="source" tone="neutral" :dot="false" :title="source === '估算' ? '该值为估算，非上游实时返回' : '该值来自缓存，可能已过期'" data-test="source">
        {{ source }}
      </StatusTag>
      <span class="pct" data-test="pct">{{ formatRemainingPct(pct) }}</span>
    </div>
    <QuotaBar :pct="pct" :thresholds="thresholds" :label="`${period.name} 剩余额度`" />
    <div class="bottom">
      <span data-test="usage">{{ usageText(period) }}<template v-if="period.unit"> <em>{{ period.unit }}</em></template></span>
      <span data-test="reset">{{ reset === null ? '重置时间未知' : reset === '待重置' ? reset : `${reset} 后重置` }}</span>
    </div>
  </div>
</template>

<style scoped>
.period { display: grid; gap: 5px; }
.top { display: flex; align-items: center; gap: 8px; min-width: 0; }
.dot { flex: none; width: 8px; height: 8px; border-radius: 50%; background: var(--c-border); }
.lv-ok .dot { background: var(--c-success); }
.lv-warn .dot { background: var(--c-warning); }
.lv-danger .dot { background: var(--c-danger); }
.name { flex: 0 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: var(--fs-small); }
.pct { margin-left: auto; font-size: var(--fs-small); font-weight: 600; font-variant-numeric: tabular-nums; }
.lv-ok .pct { color: var(--c-success); }
.lv-warn .pct { color: var(--c-warning); }
.lv-danger .pct { color: var(--c-danger); }
.lv-unknown .pct { color: var(--c-text-2); font-weight: 500; }
.bottom { display: flex; justify-content: space-between; gap: 8px; color: var(--c-text-2); font-size: var(--fs-caption); font-variant-numeric: tabular-nums; }
.bottom em { font-style: normal; opacity: 0.8; }
</style>
