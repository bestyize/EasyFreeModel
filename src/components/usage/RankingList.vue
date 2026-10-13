<script setup lang="ts">
import EmptyState from '@/components/EmptyState.vue'
import ProviderAvatar from './ProviderAvatar.vue'
import { formatPct } from './format'

export interface RankingItem {
  key: string
  label: string
  /** 头像取色与首字母来源（提供商 id） */
  providerId: string
  providerName: string
  /** 右侧已格式化的数值（如 62.4M / 86） */
  valueText: string
  pct: number
}

defineProps<{ title: string; items: RankingItem[]; emptyText?: string; showRank?: boolean }>()
</script>

<template>
  <section class="card rank-card" :aria-label="title">
    <h2 class="section-title">{{ title }}</h2>
    <ol v-if="items.length" class="list">
      <li v-for="(it, idx) in items" :key="it.key">
        <span v-if="showRank" class="rank" :class="{ top: idx === 0 }">{{ idx + 1 }}</span>
        <ProviderAvatar :id="it.providerId" :name="it.providerName" :size="22" />
        <div class="main">
          <div class="line">
            <span class="label" :title="it.label">{{ it.label }}</span>
            <span class="val">{{ it.valueText }}</span>
            <span class="pct">{{ formatPct(it.pct) }}</span>
          </div>
          <div class="track"><div class="fill" :style="{ width: `${Math.min(100, Math.max(0, it.pct))}%` }" /></div>
        </div>
      </li>
    </ol>
    <EmptyState v-else :title="emptyText ?? '暂无数据'" />
  </section>
</template>

<style scoped>
.rank-card { padding: 16px 18px; min-width: 0; }
.list { margin: 12px 0 0; padding: 0; list-style: none; display: grid; gap: 12px; }
li { display: flex; align-items: center; gap: 10px; }
.rank { flex: none; display: grid; place-items: center; width: 22px; height: 22px; border-radius: 6px; background: var(--c-surface-2); color: var(--c-text-2); font-size: var(--fs-caption); font-weight: 600; }
.rank.top { background: var(--c-brand); color: #fff; }
.main { flex: 1; min-width: 0; }
.line { display: grid; grid-template-columns: minmax(0, 1fr) auto 52px; gap: 10px; align-items: baseline; font-size: var(--fs-small); font-variant-numeric: tabular-nums; }
.label { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.val { color: var(--c-text); }
.pct { text-align: right; color: var(--c-text-2); }
.track { height: 5px; margin-top: 5px; border-radius: 3px; background: var(--c-surface-2); overflow: hidden; }
.fill { height: 100%; border-radius: 3px; background: var(--c-brand); }
</style>
