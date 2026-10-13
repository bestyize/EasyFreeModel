<script setup lang="ts">
import { computed, ref } from 'vue'
import { ChevronDown, ChevronUp, Coins, RefreshCw, TriangleAlert } from 'lucide-vue-next'
import type { QuotaAccount } from '@/types'
import StatusTag from '@/components/StatusTag.vue'
import ProviderAvatar from '@/components/usage/ProviderAvatar.vue'
import QuotaPeriodRow from './QuotaPeriodRow.vue'
import ToggleSwitch from './ToggleSwitch.vue'
import { COLLAPSED_PERIODS, DEFAULT_THRESHOLDS, formatBalance, formatSyncedAt, type QuotaThresholds } from './quota'

const props = withDefaults(
  defineProps<{
    account: QuotaAccount
    now: number
    syncing?: boolean
    toggling?: boolean
    thresholds?: QuotaThresholds
  }>(),
  { syncing: false, toggling: false, thresholds: () => DEFAULT_THRESHOLDS },
)
const emit = defineEmits<{ sync: [connectionId: string]; toggle: [connectionId: string, enabled: boolean] }>()

const expanded = ref(false)
const shownPeriods = computed(() =>
  expanded.value ? props.account.periods : props.account.periods.slice(0, COLLAPSED_PERIODS),
)
const hiddenCount = computed(() => Math.max(0, props.account.periods.length - COLLAPSED_PERIODS))
const noData = computed(() => props.account.periods.length === 0 && props.account.balances.length === 0)
</script>

<template>
  <article class="quota-card card" :class="{ disabled: !account.enabled }" :data-connection="account.connectionId">
    <header>
      <ProviderAvatar :id="account.providerId" :name="account.providerName" :size="40" />
      <div class="title">
        <h3>{{ account.providerName }}</h3>
        <p :title="account.accountLabel">{{ account.accountLabel }}</p>
      </div>
      <StatusTag v-if="!account.enabled" tone="neutral" :dot="false">已停用</StatusTag>
      <button class="icon-btn" type="button" :disabled="syncing" :aria-label="`同步 ${account.providerName} ${account.accountLabel}`" data-test="sync" @click="emit('sync', account.connectionId)">
        <RefreshCw :size="16" :class="{ spin: syncing }" />
      </button>
      <ToggleSwitch
        :model-value="account.enabled"
        :label="`${account.enabled ? '停用' : '启用'} ${account.providerName} ${account.accountLabel}`"
        :disabled="toggling"
        @change="(v: boolean) => emit('toggle', account.connectionId, v)"
      />
    </header>

    <p class="count" v-if="account.periods.length > 0">{{ account.periods.length }} 个额度周期</p>

    <div class="periods">
      <QuotaPeriodRow v-for="(p, i) in shownPeriods" :key="`${p.name}-${i}`" :period="p" :now="now" :thresholds="thresholds" />
    </div>
    <button v-if="hiddenCount > 0" class="more" type="button" @click="expanded = !expanded">
      <component :is="expanded ? ChevronUp : ChevronDown" :size="14" />
      {{ expanded ? '收起' : `展开其余 ${hiddenCount} 项` }}
    </button>

    <ul v-if="account.balances.length > 0" class="balances">
      <li v-for="b in account.balances" :key="b.currency" data-test="balance">
        <Coins :size="15" aria-hidden="true" />
        <span>余额 ({{ b.currency }})</span>
        <b :class="{ unknown: b.amount === null, zero: b.amount === 0 }">{{ formatBalance(b.amount, b.currency) }}</b>
      </li>
    </ul>

    <p v-if="noData" class="nodata">暂无额度数据</p>

    <footer>
      <p v-if="account.error" class="error" role="alert" data-test="error">
        <TriangleAlert :size="14" />
        <span>同步失败：{{ account.error }}</span>
      </p>
      <p class="synced" data-test="synced">
        <template v-if="account.lastSyncedAt">上次成功同步 {{ formatSyncedAt(account.lastSyncedAt, now) }}</template>
        <template v-else>尚未同步成功</template>
      </p>
    </footer>
  </article>
</template>

<style scoped>
.quota-card { display: grid; gap: 12px; padding: 14px 16px; min-width: 0; }
.quota-card.disabled .periods, .quota-card.disabled .balances { opacity: 0.55; }
header { display: flex; align-items: center; gap: 10px; }
.title { flex: 1; min-width: 0; }
h3 { margin: 0; font-size: var(--fs-body); font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.title p { margin: 2px 0 0; color: var(--c-text-2); font-size: var(--fs-small); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.icon-btn { display: grid; place-items: center; width: 32px; height: 32px; border: 0; border-radius: 8px; background: transparent; color: var(--c-text-2); }
.icon-btn:hover:not(:disabled) { background: var(--c-surface-2); color: var(--c-text); }
.icon-btn:disabled { cursor: progress; }
.spin { animation: spin 0.9s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
.count { margin: -4px 0 0; width: fit-content; padding: 1px 8px; border-radius: 10px; background: var(--c-surface-2); color: var(--c-text-2); font-size: var(--fs-caption); }
.periods { display: grid; gap: 12px; }
.more { display: inline-flex; align-items: center; gap: 4px; justify-self: start; padding: 2px 4px; border: 0; background: transparent; color: var(--c-text-2); font-size: var(--fs-caption); }
.more:hover { color: var(--c-brand); }
.balances { display: grid; gap: 8px; margin: 0; padding: 0; list-style: none; }
.balances li { display: flex; align-items: center; gap: 8px; font-size: var(--fs-small); color: var(--c-text-2); }
.balances b { margin-left: auto; color: var(--c-text); font-variant-numeric: tabular-nums; }
.balances b.zero { color: var(--c-danger); }
.balances b.unknown { color: var(--c-text-2); font-weight: 500; }
.nodata { margin: 0; color: var(--c-text-2); font-size: var(--fs-small); }
footer { display: grid; gap: 6px; }
footer p { margin: 0; }
.error { display: flex; align-items: flex-start; gap: 6px; padding: 6px 10px; border-radius: 8px; background: #fdecec; color: var(--c-danger); font-size: var(--fs-caption); line-height: 1.5; overflow-wrap: anywhere; }
.error svg { flex: none; margin-top: 2px; }
.synced { color: var(--c-text-2); font-size: var(--fs-caption); }
</style>
