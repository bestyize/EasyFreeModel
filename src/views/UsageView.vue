<script setup lang="ts">
import { computed, onActivated, onMounted } from 'vue'
import { Coins, FileText, Layers, LogIn, LogOut } from 'lucide-vue-next'
import EmptyState from '@/components/EmptyState.vue'
import DonutChart from '@/components/usage/DonutChart.vue'
import MetricCard from '@/components/usage/MetricCard.vue'
import RankingList, { type RankingItem } from '@/components/usage/RankingList.vue'
import RequestTable from '@/components/usage/RequestTable.vue'
import UsageChart from '@/components/usage/UsageChart.vue'
import UsageDetail from '@/components/usage/UsageDetail.vue'
import { RANGE_OPTIONS, compareLabel, formatCompact, formatCost, formatInt } from '@/components/usage/format'
import { useUsageStore, type UsageTab } from '@/stores/usage'

const store = useUsageStore()

const TABS: { value: UsageTab; label: string }[] = [
  { value: 'overview', label: '概览' },
  { value: 'detail', label: '详情' },
]

const summary = computed(() => store.summary)
const compare = computed(() => compareLabel(store.range))
const isEmpty = computed(
  () => summary.value !== null && summary.value.requests.value === 0 && summary.value.trend.length === 0,
)

const modelItems = computed<RankingItem[]>(() =>
  (summary.value?.modelRanking ?? []).map((m) => ({
    key: `${m.providerId}/${m.model}`,
    label: m.model,
    providerId: m.providerId,
    providerName: store.providerNames[m.providerId] ?? m.providerId,
    valueText: formatCompact(m.tokens),
    pct: m.pct,
  })),
)
const distributionItems = computed<RankingItem[]>(() =>
  (summary.value?.requestDistribution ?? []).map((d) => ({
    key: d.providerId,
    label: d.name,
    providerId: d.providerId,
    providerName: d.name,
    valueText: formatInt(d.requests),
    pct: d.pct,
  })),
)

onMounted(() => void store.load())
// KeepAlive 下重新进入页面时刷新一次（首次激活已由 onMounted 加载）
let activatedOnce = false
onActivated(() => {
  if (!activatedOnce) {
    activatedOnce = true
    return
  }
  void store.load(true)
})
</script>

<template>
  <div class="usage-page">
    <div class="toolbar">
      <div class="seg" role="tablist" aria-label="使用情况视图">
        <button
          v-for="t in TABS"
          :key="t.value"
          type="button"
          role="tab"
          :aria-selected="store.tab === t.value"
          :class="{ active: store.tab === t.value }"
          @click="store.setTab(t.value)"
        >
          {{ t.label }}
        </button>
      </div>
      <div class="seg" role="tablist" aria-label="时间维度">
        <button
          v-for="r in RANGE_OPTIONS"
          :key="r.value"
          type="button"
          role="tab"
          :aria-selected="store.range === r.value"
          :class="{ active: store.range === r.value }"
          @click="store.setRange(r.value)"
        >
          {{ r.label }}
        </button>
      </div>
    </div>

    <UsageDetail v-if="store.tab === 'detail'" />

    <template v-else>
      <p v-if="store.summaryStatus === 'loading' && !summary" class="state" role="status">加载中…</p>
      <EmptyState v-else-if="store.summaryStatus === 'error' && !summary" title="使用情况加载失败" :description="store.summaryError ?? ''" dashed>
        <button class="btn btn-primary" type="button" @click="store.load(true)">重试</button>
      </EmptyState>
      <template v-else-if="summary">
        <p v-if="store.summaryStatus === 'error'" class="banner" role="alert">
          刷新失败：{{ store.summaryError }}（显示的是上次结果）
          <button class="btn" type="button" @click="store.load(true)">重试</button>
        </p>
        <EmptyState v-if="isEmpty" title="该时间范围暂无使用数据" description="通过本地代理发起请求后，这里会显示 Token、成本和趋势。" dashed />
        <div v-else class="content" :class="{ stale: store.summaryStatus === 'loading' }" :aria-busy="store.summaryStatus === 'loading'">
          <div class="metrics">
            <MetricCard label="请求总数" :value="formatInt(summary.requests.value)" :delta-pct="summary.requests.deltaPct" :compare-text="compare" :icon="FileText" icon-color="#3b82f6" />
            <MetricCard label="输入 Token" :value="formatInt(summary.inputTokens.value)" :delta-pct="summary.inputTokens.deltaPct" :compare-text="compare" :icon="LogIn" icon-color="#ff2442" />
            <MetricCard label="输出 Token" :value="formatInt(summary.outputTokens.value)" :delta-pct="summary.outputTokens.deltaPct" :compare-text="compare" :icon="LogOut" icon-color="#16a36b" />
            <MetricCard
              label="缓存 Token"
              :value="formatInt(summary.cachedTokens.value)"
              :delta-pct="summary.cachedTokens.deltaPct"
              :compare-text="compare"
              :icon="Layers"
              icon-color="#8b5cf6"
              note="独立展示，未与输入 Token 相加"
            />
            <MetricCard
              label="估算成本"
              :value="formatCost(summary.estimatedCost.value)"
              :delta-pct="summary.estimatedCost.deltaPct"
              :compare-text="compare"
              :icon="Coins"
              icon-color="#f4a524"
              :positive-is-good="false"
              :note="`估算 · ${summary.costSource}`"
            />
          </div>

          <div class="main-row">
            <UsageChart :trend="summary.trend" />
            <section class="card recent" aria-label="最近请求">
              <header>
                <h2 class="section-title">最近请求</h2>
                <button class="btn" type="button" @click="store.setTab('detail')">查看全部</button>
              </header>
              <p v-if="store.recentStatus === 'loading' && store.recent.length === 0" class="state small" role="status">加载中…</p>
              <EmptyState v-else-if="store.recentStatus === 'error' && store.recent.length === 0" title="最近请求加载失败" :description="store.recentError ?? ''">
                <button class="btn" type="button" @click="store.loadRecent()">重试</button>
              </EmptyState>
              <EmptyState v-else-if="store.recent.length === 0" title="暂无请求" />
              <RequestTable v-else :rows="store.recent" :provider-names="store.providerNames" compact />
            </section>
          </div>

          <div class="bottom-row">
            <DonutChart :items="summary.providerShare" />
            <RankingList title="模型使用排行" :items="modelItems" show-rank empty-text="暂无模型数据" />
            <RankingList title="请求分布" :items="distributionItems" empty-text="暂无请求分布" />
          </div>
        </div>
      </template>
    </template>
  </div>
</template>

<style scoped>
.usage-page { display: flex; flex-direction: column; gap: var(--card-gap); }
.toolbar { display: flex; align-items: center; justify-content: space-between; gap: 12px; flex-wrap: wrap; }
.seg { display: inline-flex; padding: 3px; border: 1px solid var(--c-border); border-radius: 8px; background: var(--c-sidebar); }
.seg button { height: 30px; padding: 0 16px; border: 0; border-radius: 6px; background: transparent; color: var(--c-text-2); font-size: var(--fs-small); }
.seg button:hover { color: var(--c-text); }
.seg button.active { background: var(--c-canvas); color: var(--c-brand); box-shadow: 0 0 0 1px var(--c-brand-soft); font-weight: 600; }
.content { display: flex; flex-direction: column; gap: var(--card-gap); }
.stale { opacity: 0.6; transition: opacity 0.15s; }
.metrics { display: grid; grid-template-columns: repeat(auto-fit, minmax(190px, 1fr)); gap: var(--card-gap); }
.main-row { display: grid; grid-template-columns: minmax(0, 1.5fr) minmax(360px, 1fr); gap: var(--card-gap); align-items: start; }
.recent { padding: 16px 18px; min-width: 0; }
.recent header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 10px; }
.bottom-row { display: grid; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); gap: var(--card-gap); align-items: start; }
.state { margin: 0; padding: 60px 0; text-align: center; color: var(--c-text-2); }
.state.small { padding: 30px 0; }
.banner { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin: 0; padding: 8px 12px; border-radius: 8px; background: #fdecec; color: var(--c-danger); font-size: var(--fs-small); }
@media (max-width: 1280px) {
  .main-row { grid-template-columns: minmax(0, 1fr); }
}
</style>
