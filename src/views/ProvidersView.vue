<script setup lang="ts">
/**
 * 提供商页。分组顺序：自定义 → OAuth → 免费额度 → API Key。
 *
 * 第四组「API Key 提供商」是对 PRODUCT_DESIGN.md §4.2「固定三组」的有意扩展：目录对齐 9Router 后共 133 家，
 * 其中 87 家（API Key 与网页会话类）放不进原三组。该组默认折叠成一行摘要“共 N 家”，展开后四列卡片，
 * 支持按能力（聊天/图像/语音/视频/搜索/嵌入）筛选，并响应顶部全局搜索；卡片显示能力小标签，
 * 适配器未接入的显示 pendingReason（卡片上省略 + title，弹窗里完整展示）。
 */
import { computed, onMounted, ref } from 'vue'
import { ChevronDown, ChevronUp, LayoutGrid, List, Play, Plus } from 'lucide-vue-next'
import EmptyState from '@/components/EmptyState.vue'
import CustomProviderModal from '@/components/providers/CustomProviderModal.vue'
import ProviderCard from '@/components/providers/ProviderCard.vue'
import ProviderModal from '@/components/providers/ProviderModal.vue'
import { CAPABILITIES, hasCapability, type CapabilityId } from '@/components/providers/status'
import { filterProviders, useProvidersStore } from '@/stores/providers'
import { useUiStore } from '@/stores/ui'
import type { CustomProtocol, ProviderGroup, ProviderSummary } from '@/types'

const store = useProvidersStore()
const ui = useUiStore()

const layout = ref<'card' | 'list'>('card')
const modalOpen = ref(false)
const activeProvider = ref<ProviderSummary | null>(null)
const customOpen = ref(false)
const customProtocol = ref<CustomProtocol>('openai-chat')

/** API Key 组：默认全部展开；支持手动收起 */
const apikeyExpanded = ref(true)
const capFilter = ref<CapabilityId | 'all'>('all')

onMounted(() => {
  if (!store.loaded) store.fetchProviders()
})

function groupItems(group: ProviderGroup) {
  return filterProviders(store.byGroup(group), ui.globalQuery)
}
const custom = computed(() => groupItems('custom'))
const oauth = computed(() => groupItems('oauth'))
const free = computed(() => groupItems('free'))
const customTotal = computed(() => store.byGroup('custom').length)
const searching = computed(() => ui.globalQuery.trim() !== '')

const apikeyAll = computed(() => store.byGroup('apikey'))
const apikeyReadyCount = computed(() => apikeyAll.value.filter((p) => p.adapterReady).length)
const apikeySearched = computed(() => groupItems('apikey'))
const apikeyShown = computed(() =>
  capFilter.value === 'all'
    ? apikeySearched.value
    : apikeySearched.value.filter((p) => hasCapability(p.serviceKinds, capFilter.value as CapabilityId)),
)
const apikeyOpen = computed(() => apikeyExpanded.value || searching.value)
const capCount = computed(() => (id: CapabilityId) => apikeySearched.value.filter((p) => hasCapability(p.serviceKinds, id)).length)

function openProvider(p: ProviderSummary) {
  activeProvider.value = p
  modalOpen.value = true
}

/** 新建自定义提供商：先在弹窗里填协议与 Base URL，由 Rust 校验并持久化 */
function addCustom(protocol: CustomProtocol) {
  customProtocol.value = protocol
  customOpen.value = true
}

const gridClass = computed(() => (layout.value === 'card' ? 'grid' : 'list'))
</script>

<template>
  <div class="providers">
    <!-- 加载 / 失败 -->
    <div v-if="store.loading && !store.loaded" class="status-block" role="status" aria-live="polite">
      <div class="skeleton-grid" aria-hidden="true">
        <div v-for="i in 8" :key="i" class="skeleton" />
      </div>
      <p>正在加载提供商…</p>
    </div>
    <EmptyState v-else-if="store.error && !store.loaded" title="提供商加载失败" :description="store.error" dashed>
      <button type="button" class="btn btn-primary" @click="store.fetchProviders()">重试</button>
    </EmptyState>

    <template v-else>
      <div v-if="store.error" class="banner" role="alert">
        刷新失败：{{ store.error }}
        <button type="button" class="btn" @click="store.fetchProviders()">重试</button>
      </div>

      <!-- 1. 自定义提供商 -->
      <section class="group" aria-labelledby="g-custom">
        <header class="group-head">
          <div>
            <h2 id="g-custom" class="section-title">自定义提供商（OpenAI/Anthropic 兼容）</h2>
            <p class="desc">添加与 OpenAI/Anthropic 兼容的自定义提供商，使用您的 API Key 或自建端点</p>
          </div>
          <div class="actions">
            <button type="button" class="btn btn-primary" aria-label="添加 Anthropic 兼容提供商" @click="addCustom('anthropic-messages')">
              <Plus :size="14" /> 添加 Anthropic 兼容
            </button>
            <button type="button" class="btn" aria-label="添加 OpenAI 兼容提供商" @click="addCustom('openai-chat')">
              <Plus :size="14" /> 添加 OpenAI 兼容
            </button>
          </div>
        </header>
        <EmptyState
          v-if="customTotal === 0"
          title="暂无自定义提供商"
          description="点击上方按钮添加第一个自定义提供商"
          dashed
        />
        <p v-else-if="!custom.length" class="no-match">没有匹配“{{ ui.globalQuery }}”的自定义提供商。</p>
        <div v-else :class="gridClass">
          <ProviderCard v-for="p in custom" :key="p.id" :provider="p" :layout="layout" @open="openProvider" />
        </div>
      </section>

      <!-- 2. OAuth 提供商 -->
      <section class="group" aria-labelledby="g-oauth">
        <header class="group-head">
          <div>
            <h2 id="g-oauth" class="section-title">OAuth 提供商</h2>
            <p class="desc">无需 API Key，通过 OAuth 快速连接各大平台</p>
          </div>
          <div class="actions">
            <div class="seg" role="group" aria-label="显示方式">
              <button
                type="button"
                :class="{ on: layout === 'card' }"
                :aria-pressed="layout === 'card'"
                aria-label="卡片视图"
                @click="layout = 'card'"
              >
                <LayoutGrid :size="16" />
              </button>
              <button
                type="button"
                :class="{ on: layout === 'list' }"
                :aria-pressed="layout === 'list'"
                aria-label="列表视图"
                @click="layout = 'list'"
              >
                <List :size="16" />
              </button>
            </div>
            <button
              type="button"
              class="btn"
              :disabled="store.testingGroup !== null"
              aria-label="测试全部 OAuth 提供商"
              @click="store.testGroup('oauth')"
            >
              <Play :size="14" /> {{ store.testingGroup === 'oauth' ? '测试中…' : '测试全部' }}
            </button>
          </div>
        </header>
        <p v-if="!oauth.length" class="no-match">{{ searching ? `没有匹配“${ui.globalQuery}”的 OAuth 提供商。` : '暂无 OAuth 提供商。' }}</p>
        <div v-else :class="gridClass">
          <ProviderCard
            v-for="p in oauth"
            :key="p.id"
            :provider="p"
            :layout="layout"
            :batch="store.batch[p.id]"
            @open="openProvider"
          />
        </div>
      </section>

      <!-- 3. 免费额度提供商 -->
      <section class="group" aria-labelledby="g-free">
        <header class="group-head">
          <div>
            <h2 id="g-free" class="section-title">免费额度提供商</h2>
            <p class="desc">提供免费额度的公共模型服务</p>
          </div>
          <div class="actions">
            <button
              type="button"
              class="btn"
              :disabled="store.testingGroup !== null"
              aria-label="测试全部免费额度提供商"
              @click="store.testGroup('free')"
            >
              <Play :size="14" /> {{ store.testingGroup === 'free' ? '测试中…' : '测试全部' }}
            </button>
          </div>
        </header>
        <p v-if="!free.length" class="no-match">{{ searching ? `没有匹配“${ui.globalQuery}”的免费额度提供商。` : '暂无免费额度提供商。' }}</p>
        <div v-else :class="gridClass">
          <ProviderCard
            v-for="p in free"
            :key="p.id"
            :provider="p"
            :layout="layout"
            :batch="store.batch[p.id]"
            @open="openProvider"
          />
        </div>
      </section>

      <!-- 4. API Key 提供商（对“固定三组”的有意扩展，见文件头注释） -->
      <section class="group" aria-labelledby="g-apikey" data-group="apikey">
        <header class="group-head">
          <div>
            <h2 id="g-apikey" class="section-title">API Key 提供商</h2>
            <p class="desc">使用各厂商的 API Key 直接接入；适配器尚未接入的会如实标注“待接入”</p>
          </div>
          <div class="actions">
            <button
              type="button"
              class="btn"
              :disabled="store.testingGroup !== null"
              aria-label="测试全部 API Key 提供商"
              @click="store.testGroup('apikey')"
            >
              <Play :size="14" /> {{ store.testingGroup === 'apikey' ? '测试中…' : '测试全部' }}
            </button>
            <button
              type="button"
              class="btn"
              :aria-expanded="apikeyOpen"
              aria-controls="apikey-body"
              :disabled="searching"
              :title="searching ? '搜索中，已自动展开' : undefined"
              @click="apikeyExpanded = !apikeyExpanded"
            >
              <component :is="apikeyOpen ? ChevronUp : ChevronDown" :size="14" /> {{ apikeyOpen ? '收起' : '展开' }}
            </button>
          </div>
        </header>

        <p v-if="store.batchNote" class="note" role="status">{{ store.batchNote }}</p>

        <div v-if="!apikeyOpen" class="summary-row" data-testid="apikey-summary">
          <strong>共 {{ apikeyAll.length }} 家</strong>
          <span>可用 {{ apikeyReadyCount }} 家 · 待接入 {{ apikeyAll.length - apikeyReadyCount }} 家</span>
        </div>

        <div v-else id="apikey-body">
          <div class="chips" role="group" aria-label="按能力筛选">
            <button type="button" class="chip" :class="{ on: capFilter === 'all' }" :aria-pressed="capFilter === 'all'" @click="capFilter = 'all'">
              全部 <em>{{ apikeySearched.length }}</em>
            </button>
            <button
              v-for="c in CAPABILITIES"
              :key="c.id"
              type="button"
              class="chip"
              :class="{ on: capFilter === c.id }"
              :aria-pressed="capFilter === c.id"
              :disabled="capCount(c.id) === 0 && capFilter !== c.id"
              @click="capFilter = capFilter === c.id ? 'all' : c.id"
            >
              {{ c.label }} <em>{{ capCount(c.id) }}</em>
            </button>
          </div>
          <p v-if="!apikeyShown.length" class="no-match">
            {{ searching ? `没有匹配“${ui.globalQuery}”的 API Key 提供商。` : '没有符合该能力的 API Key 提供商。' }}
          </p>
          <div v-else class="grid" data-testid="apikey-grid">
            <ProviderCard
              v-for="p in apikeyShown"
              :key="p.id"
              :provider="p"
              layout="card"
              detailed
              :batch="store.batch[p.id]"
              @open="openProvider"
            />
          </div>
        </div>
      </section>
    </template>

    <ProviderModal v-model="modalOpen" :provider="activeProvider" />
    <CustomProviderModal v-model="customOpen" :initial-protocol="customProtocol" />
  </div>
</template>

<style scoped>
.providers { display: flex; flex-direction: column; gap: var(--section-gap); }
.group-head { display: flex; align-items: flex-end; justify-content: space-between; gap: 16px; margin-bottom: 12px; }
.desc { margin: 4px 0 0; font-size: var(--fs-small); color: var(--c-text-2); }
.actions { display: flex; align-items: center; gap: 10px; flex: none; }
.grid { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: var(--card-gap); }
@media (max-width: 1360px) { .grid { grid-template-columns: repeat(3, minmax(0, 1fr)); } }
@media (max-width: 1100px) { .grid { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
.list { display: flex; flex-direction: column; gap: var(--card-gap); }
.note { margin: 0 0 8px; font-size: var(--fs-small); color: var(--c-text-2); }
.summary-row { display: flex; align-items: center; gap: 12px; height: 48px; padding: 0 16px; border: 1px solid var(--c-border); border-radius: var(--radius); background: var(--c-sidebar); font-size: var(--fs-small); color: var(--c-text-2); }
.summary-row strong { font-size: var(--fs-body); color: var(--c-text); }
.chips { display: flex; flex-wrap: wrap; gap: 8px; margin-bottom: 12px; }
.chip { display: inline-flex; align-items: center; gap: 6px; height: 30px; padding: 0 12px; border: 1px solid var(--c-border); border-radius: 15px; background: var(--c-canvas); color: var(--c-text-2); font-size: var(--fs-small); }
.chip em { font-style: normal; font-size: 12px; color: var(--c-text-2); }
.chip:hover:not(:disabled) { background: var(--c-sidebar); }
.chip:disabled { opacity: 0.5; cursor: not-allowed; }
.chip.on { border-color: var(--c-brand); background: var(--c-brand-soft); color: var(--c-brand); }
.chip.on em { color: var(--c-brand); }
.no-match { margin: 0; padding: 16px 0; font-size: var(--fs-small); color: var(--c-text-2); }
.seg { display: inline-flex; padding: 3px; border: 1px solid var(--c-border); border-radius: 8px; background: var(--c-sidebar); }
.seg button { display: grid; place-items: center; width: 32px; height: 28px; border: 0; border-radius: 6px; background: transparent; color: var(--c-text-2); }
.seg button.on { background: var(--c-canvas); color: var(--c-brand); box-shadow: 0 0 0 1px var(--c-border); }
.banner { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 14px; border-radius: var(--radius); background: #fdecec; color: var(--c-danger); font-size: var(--fs-small); }
.status-block { display: flex; flex-direction: column; gap: 12px; color: var(--c-text-2); font-size: var(--fs-small); }
.status-block p { margin: 0; }
.skeleton-grid { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: var(--card-gap); }
.skeleton { height: var(--provider-card-h); border-radius: var(--radius); background: var(--c-surface-2); }
</style>
