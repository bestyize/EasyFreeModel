<script setup lang="ts">
import { computed, ref } from 'vue'
import { errorMessage } from '@/components/media/format'
import ToggleSwitch from '@/components/settings/ToggleSwitch.vue'
import SettingsRow from '@/components/settings/SettingsRow.vue'
import { useProvidersStore } from '@/stores/providers'
import { useProxyPoolStore } from '@/stores/proxyPool'
import { SCOPE_OPTIONS, STRATEGY_OPTIONS } from './format'

const store = useProxyPoolStore()
const providers = useProvidersStore()
const draft = computed(() => store.draft!)
const saveError = ref<string | null>(null)
const providerQuery = ref('')

const strategyHint = computed(() => STRATEGY_OPTIONS.find((o) => o.id === draft.value.strategy)?.description ?? '')
const providerList = computed(() => {
  const q = providerQuery.value.trim().toLowerCase()
  const list = providers.providers.filter((p) => p.serviceKinds.includes('llm') || p.serviceKinds.length === 0 || draft.value.providerIds.includes(p.id))
  return q ? list.filter((p) => p.name.toLowerCase().includes(q) || p.id.toLowerCase().includes(q)) : list
})

function toggleProvider(id: string, on: boolean) {
  const set = new Set(draft.value.providerIds)
  if (on) set.add(id)
  else set.delete(id)
  draft.value.providerIds = [...set]
}

function numeric(e: Event, key: 'failureThreshold' | 'cooldownSeconds' | 'maxAttempts') {
  const n = Number((e.target as HTMLInputElement).value)
  draft.value[key] = Number.isFinite(n) ? n : 0
}

async function save() {
  saveError.value = null
  try {
    await store.saveConfig()
  } catch (e) {
    saveError.value = `保存失败：${errorMessage(e)}`
  }
}
</script>

<template>
  <section v-if="store.draft" class="card section" aria-labelledby="px-config">
    <header class="head">
      <h2 id="px-config" class="section-title">选路与容错</h2>
      <div class="head-actions">
        <button v-if="store.dirty" class="btn" type="button" @click="store.resetDraft()">放弃修改</button>
        <button class="btn btn-primary" type="button" data-testid="save-config" :disabled="!store.dirty || store.savingConfig" @click="save">
          {{ store.savingConfig ? '保存中…' : '保存配置' }}
        </button>
      </div>
    </header>
    <p v-if="saveError" class="err" role="alert" data-testid="config-error">{{ saveError }}</p>

    <SettingsRow label="选路策略" :description="strategyHint" for-id="px-strategy">
      <select id="px-strategy" v-model="draft.strategy" class="input" data-testid="strategy">
        <option v-for="o in STRATEGY_OPTIONS" :key="o.id" :value="o.id">{{ o.label }}</option>
      </select>
    </SettingsRow>

    <SettingsRow label="生效范围" description="选择哪些提供商的上游请求（以及连接测试、拉取模型、配额查询）经过代理池，其余直连。" for-id="px-scope">
      <select id="px-scope" v-model="draft.providerScope" class="input" data-testid="scope">
        <option v-for="o in SCOPE_OPTIONS" :key="o.id" :value="o.id">{{ o.label }}</option>
      </select>
      <template #extra>
        <div v-if="draft.providerScope !== 'all'" class="providers" data-testid="provider-picker">
          <input v-model="providerQuery" class="input search" placeholder="搜索提供商" aria-label="搜索提供商" />
          <p class="picked">已选 {{ draft.providerIds.length }} 个{{ draft.providerScope === 'include' ? '（只有它们走代理）' : '（它们直连）' }}</p>
          <ul>
            <li v-for="p in providerList" :key="p.id">
              <label>
                <input type="checkbox" :checked="draft.providerIds.includes(p.id)" :data-testid="`pick-${p.id}`" @change="toggleProvider(p.id, ($event.target as HTMLInputElement).checked)" />
                {{ p.name }}
              </label>
            </li>
            <li v-if="!providerList.length" class="none">没有匹配的提供商</li>
          </ul>
        </div>
      </template>
    </SettingsRow>

    <SettingsRow label="单次请求最多尝试的代理数" description="连不上某个代理时自动换下一个，最多换这么多个，1–5。" for-id="px-attempts">
      <input id="px-attempts" class="input narrow" type="number" min="1" max="5" :value="draft.maxAttempts" @input="numeric($event, 'maxAttempts')" />
    </SettingsRow>

    <SettingsRow label="连续失败阈值" description="节点连续连接失败达到该次数后进入冷却，1–20。" for-id="px-threshold">
      <input id="px-threshold" class="input narrow" type="number" min="1" max="20" :value="draft.failureThreshold" @input="numeric($event, 'failureThreshold')" />
    </SettingsRow>

    <SettingsRow label="冷却时间（秒）" description="冷却期间不再选用该节点；到期后放行一次试探，再失败会立刻重新冷却，5–3600。" for-id="px-cooldown">
      <input id="px-cooldown" class="input narrow" type="number" min="5" max="3600" :value="draft.cooldownSeconds" @input="numeric($event, 'cooldownSeconds')" />
    </SettingsRow>

    <SettingsRow
      label="无可用代理时回退直连"
      description="关闭（推荐）：没有可用节点时拒绝出站，不会悄悄暴露本机 IP。开启：改为直连上游。"
    >
      <ToggleSwitch v-model="draft.fallbackDirect" label="无可用代理时回退直连" />
    </SettingsRow>

    <SettingsRow label="测速地址" description="测速时经代理访问的地址，任何正常响应都算连通；若返回 ip= 字段还会显示出口 IP。" for-id="px-test-url">
      <input id="px-test-url" v-model="draft.testUrl" class="input wide" spellcheck="false" />
    </SettingsRow>
  </section>
</template>

<style scoped>
.section { padding: 20px 24px; }
.head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 4px; }
.head-actions { display: flex; gap: 8px; }
.narrow { width: 120px; }
.wide { width: 340px; }
.err { margin: 4px 0 0; padding: 8px 12px; border-radius: 8px; background: var(--c-danger-soft); color: var(--c-danger); font-size: var(--fs-small); }
.providers { margin-top: 10px; max-width: 460px; }
.search { width: 100%; }
.picked { margin: 6px 0; color: var(--c-text-2); font-size: var(--fs-caption); }
ul { margin: 0; padding: 4px 10px; max-height: 180px; overflow-y: auto; list-style: none; border: 1px solid var(--c-border); border-radius: 8px; }
li label { display: flex; align-items: center; gap: 8px; padding: 4px 0; font-size: var(--fs-small); }
.none { padding: 8px 0; color: var(--c-text-2); font-size: var(--fs-small); }
</style>
