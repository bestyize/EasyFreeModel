<script setup lang="ts">
import { computed } from 'vue'
import { formatRemainingPct, quotaLevel, type QuotaThresholds, DEFAULT_THRESHOLDS } from './quota'

const props = withDefaults(
  defineProps<{ pct: number | null; thresholds?: QuotaThresholds; label?: string }>(),
  { thresholds: () => DEFAULT_THRESHOLDS, label: '剩余额度' },
)

const level = computed(() => quotaLevel(props.pct, props.thresholds))
</script>

<template>
  <div
    class="bar"
    :class="`lv-${level}`"
    role="progressbar"
    :aria-label="label"
    aria-valuemin="0"
    aria-valuemax="100"
    :aria-valuenow="pct ?? undefined"
    :aria-valuetext="pct === null ? '未知' : formatRemainingPct(pct)"
  >
    <!-- 未知时只显示空轨道，绝不画一条 0 宽或满宽的“假”进度 -->
    <div v-if="pct !== null" class="fill" :style="{ width: `${pct}%` }" data-test="fill" />
  </div>
</template>

<style scoped>
.bar { height: 6px; border-radius: 3px; background: var(--c-surface-2); overflow: hidden; }
.fill { height: 100%; border-radius: 3px; }
.lv-ok .fill { background: var(--c-success); }
.lv-warn .fill { background: var(--c-warning); }
.lv-danger .fill { background: var(--c-danger); }
.lv-unknown { border: 1px dashed var(--c-border); background: var(--c-canvas); }
</style>
