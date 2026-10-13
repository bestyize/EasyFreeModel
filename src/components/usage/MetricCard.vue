<script setup lang="ts">
import { computed, type Component } from 'vue'
import { ArrowDownRight, ArrowUpRight, Minus } from 'lucide-vue-next'
import { formatDelta } from './format'

const props = withDefaults(
  defineProps<{
    label: string
    /** 已格式化的主数值 */
    value: string
    /** 较上一周期变化百分比；null = 无对比数据，不显示涨跌 */
    deltaPct: number | null
    /** 对比基准文案，如“较昨日” */
    compareText?: string
    icon: Component
    iconColor?: string
    /** 数值上升是否为好事（成本上升不是） */
    positiveIsGood?: boolean
    /** 补充说明（如估算来源、缓存口径） */
    note?: string
  }>(),
  { compareText: '', iconColor: '#677084', positiveIsGood: true, note: '' },
)

const direction = computed<'up' | 'down' | 'flat' | null>(() => {
  if (props.deltaPct === null || !Number.isFinite(props.deltaPct)) return null
  if (Math.round(props.deltaPct) === 0) return 'flat'
  return props.deltaPct > 0 ? 'up' : 'down'
})

const tone = computed(() => {
  if (direction.value === null || direction.value === 'flat') return 'neutral'
  const good = (direction.value === 'up') === props.positiveIsGood
  return good ? 'good' : 'bad'
})
</script>

<template>
  <section class="metric card" :aria-label="label">
    <span class="icon" :style="{ color: iconColor }" aria-hidden="true">
      <component :is="icon" :size="20" />
    </span>
    <div class="body">
      <p class="label">{{ label }}</p>
      <p class="value">{{ value }}</p>
      <p v-if="direction" class="delta" :class="`tone-${tone}`" data-test="delta">
        <ArrowUpRight v-if="direction === 'up'" :size="14" />
        <ArrowDownRight v-else-if="direction === 'down'" :size="14" />
        <Minus v-else :size="14" />
        <b>{{ formatDelta(deltaPct as number) }}</b>
        <span v-if="compareText" class="compare">{{ compareText }}</span>
      </p>
      <p v-if="note" class="note" :title="note" data-test="note">{{ note }}</p>
    </div>
  </section>
</template>

<style scoped>
.metric { display: flex; gap: 12px; padding: 14px 16px; min-width: 0; }
.icon { flex: none; display: grid; place-items: center; width: 40px; height: 40px; border-radius: 10px; background: var(--c-surface-2); }
.body { min-width: 0; }
p { margin: 0; }
.label { color: var(--c-text-2); font-size: var(--fs-small); }
.value { margin-top: 2px; font-size: 22px; font-weight: 600; font-variant-numeric: tabular-nums; overflow-wrap: anywhere; }
.delta { display: flex; align-items: center; gap: 3px; margin-top: 4px; font-size: var(--fs-caption); }
.delta b { font-weight: 600; }
.compare { margin-left: 4px; color: var(--c-text-2); }
.tone-good { color: var(--c-success); }
.tone-bad { color: var(--c-danger); }
.tone-neutral { color: var(--c-text-2); }
.note { margin-top: 4px; color: var(--c-text-2); font-size: var(--fs-caption); line-height: 1.4; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
</style>
