<script setup lang="ts">
import { computed } from 'vue'
import StatusTag from '@/components/StatusTag.vue'
import { useSettingsStore } from '@/stores/settings'
import { PROTOCOL_OPTIONS } from './protocols'
import SettingsRow from './SettingsRow.vue'
import ToggleSwitch from './ToggleSwitch.vue'
import type { SettingsErrors } from './validation'

defineProps<{ errors: SettingsErrors }>()
const settings = useSettingsStore()
const draft = computed(() => settings.draft!)
</script>

<template>
  <section class="card section" aria-labelledby="set-routing">
    <h2 id="set-routing" class="section-title">协议与路由</h2>

    <SettingsRow label="请求超时（秒）" description="上游请求的最长等待时间，1–600。" :error="errors.timeoutSeconds" for-id="s-timeout">
      <input id="s-timeout" v-model.number="draft.timeoutSeconds" class="input narrow" type="number" min="1" max="600" />
    </SettingsRow>
    <SettingsRow label="失败重试次数" description="上游失败时的重试次数，0–10。" :error="errors.retryCount" for-id="s-retry">
      <input id="s-retry" v-model.number="draft.retryCount" class="input narrow" type="number" min="0" max="10" />
    </SettingsRow>

    <div class="protocols">
      <h3>对外协议</h3>
      <p class="desc">仅展示适配器实现状态，开关由后端实际能力决定，当前不可修改。</p>
      <ul>
        <li v-for="p in PROTOCOL_OPTIONS" :key="p.id" :data-testid="`protocol-${p.id}`">
          <span>{{ p.label }}</span>
          <span class="right">
            <StatusTag :tone="p.implemented ? 'success' : 'neutral'">{{ p.implemented ? '已实现' : '尚未实现' }}</StatusTag>
            <ToggleSwitch :model-value="p.implemented" disabled :label="`${p.label} 协议开关`" />
          </span>
        </li>
      </ul>
    </div>
  </section>
</template>

<style scoped>
.section { padding: 20px 24px; }
.narrow { width: 120px; }
.protocols { padding-top: 14px; border-top: 1px solid var(--c-border); }
h3 { margin: 0; font-size: var(--fs-body); font-weight: 500; }
.desc { margin: 2px 0 8px; color: var(--c-text-2); font-size: var(--fs-caption); }
ul { margin: 0; padding: 0; list-style: none; }
li { display: flex; align-items: center; justify-content: space-between; padding: 8px 0; }
.right { display: flex; align-items: center; gap: 10px; }
</style>
