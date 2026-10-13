<script setup lang="ts">
import { computed } from 'vue'
import { ChevronRight } from 'lucide-vue-next'
import StatusTag from '@/components/StatusTag.vue'
import ProviderLogo from './ProviderLogo.vue'
import { capabilityLabels, protocolText, providerStatus } from './status'
import type { BatchState } from '@/stores/providers'
import type { ProviderSummary } from '@/types'

const props = withDefaults(
  defineProps<{
    provider: ProviderSummary
    layout?: 'card' | 'list'
    batch?: BatchState
    /** 显示能力小标签与“待接入”原因（API Key 提供商组使用） */
    detailed?: boolean
  }>(),
  { layout: 'card', detailed: false },
)
const emit = defineEmits<{ open: [provider: ProviderSummary] }>()

const status = computed(() => {
  const base = providerStatus(props.provider)
  // 批量测试的瞬时结果只叠加在已接入适配器之上
  if (props.provider.adapterReady && props.batch) {
    if (props.batch.phase === 'testing') return { text: '测试中', tone: 'warning' as const }
    const { ok, total } = props.batch
    if (total > 0 && ok < total) return { text: `测试失败 ${total - ok}/${total}`, tone: 'danger' as const }
    if (total > 0) return { text: `测试通过 ${ok}/${total}`, tone: 'success' as const }
  }
  return base
})
const capabilities = computed(() => (props.detailed ? capabilityLabels(props.provider.serviceKinds) : []))
/** 未就绪时的原因：卡片上省略显示，完整文案在 title 与弹窗里 */
const reason = computed(() => (props.detailed && !props.provider.adapterReady ? props.provider.pendingReason : null))
const ariaLabel = computed(
  () => `${props.provider.name}，${status.value.text}${reason.value ? `（${reason.value}）` : ''}，打开设置`,
)
</script>

<template>
  <button type="button" class="provider-card" :class="[`layout-${layout}`]" :aria-label="ariaLabel" @click="emit('open', provider)">
    <ProviderLogo :name="provider.name" :size="36" :color="provider.color" :text-icon="provider.textIcon" />
    <span class="main">
      <span class="name" :title="provider.name">{{ provider.name }}</span>
      <span class="sub">
        <StatusTag :tone="status.tone">{{ status.text }}</StatusTag>
        <span v-if="capabilities.length" class="caps" aria-label="能力">
          <span v-for="c in capabilities" :key="c" class="cap">{{ c }}</span>
        </span>
        <span v-else-if="layout === 'card' && provider.quotaHint" class="hint">{{ provider.quotaHint }}</span>
      </span>
      <span v-if="reason" class="reason" :title="reason">{{ reason }}</span>
    </span>
    <span v-if="layout === 'list'" class="meta">
      <span>{{ provider.adapterReady ? `${provider.connectionCount} 个连接` : '—' }}</span>
      <span>{{ provider.adapterReady ? protocolText(provider.protocols) : '—' }}</span>
      <span class="quota">{{ provider.quotaHint ?? '配额未知' }}</span>
    </span>
    <ChevronRight :size="16" class="chev" aria-hidden="true" />
  </button>
</template>

<style scoped>
.provider-card {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  height: var(--provider-card-h);
  padding: 0 14px 0 12px;
  border: 1px solid var(--c-border);
  border-radius: var(--radius);
  background: var(--c-canvas);
  text-align: left;
  min-width: 0;
}
.provider-card:hover { background: var(--c-sidebar); }
.main { display: flex; flex-direction: column; gap: 2px; min-width: 0; flex: 1; }
.name { font-size: 14px; font-weight: 600; line-height: 18px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.sub { display: flex; align-items: center; gap: 8px; min-width: 0; font-size: 12px; }
.sub :deep(.status-tag) { height: 18px; padding: 0 6px; font-size: 12px; }
.caps { display: flex; gap: 4px; min-width: 0; overflow: hidden; }
.cap { flex: none; height: 18px; padding: 0 6px; border-radius: 4px; background: var(--c-surface-2); color: var(--c-text-2); font-size: 11px; line-height: 18px; }
.reason { font-size: 11px; line-height: 14px; color: var(--c-text-2); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.hint { color: var(--c-text-2); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.meta { display: flex; gap: 24px; flex: none; color: var(--c-text-2); font-size: 12px; }
.meta > span { min-width: 96px; }
.meta .quota { min-width: 140px; }
.chev { flex: none; color: var(--c-text-2); }
</style>
