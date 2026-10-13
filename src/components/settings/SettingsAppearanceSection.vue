<script setup lang="ts">
import { computed } from 'vue'
import { useSettingsStore } from '@/stores/settings'
import SettingsRow from './SettingsRow.vue'

const settings = useSettingsStore()
const draft = computed(() => settings.draft!)

const SCALES = [0.9, 1, 1.1, 1.25, 1.5]
const scaleOptions = computed(() => {
  const cur = draft.value.fontScale
  return SCALES.includes(cur) ? SCALES : [...SCALES, cur].sort((a, b) => a - b)
})
</script>

<template>
  <section class="card section" aria-labelledby="set-ui">
    <h2 id="set-ui" class="section-title">界面</h2>
    <SettingsRow label="语言" description="界面文案目前仅有简体中文，选择 English 只保存偏好，暂不生效。" for-id="s-lang">
      <select id="s-lang" v-model="draft.language" class="input">
        <option value="zh-CN">简体中文</option>
        <option value="en">English</option>
      </select>
    </SettingsRow>
    <SettingsRow label="主题" description="首版优先浅色，深色样式后续补全。" for-id="s-theme">
      <select id="s-theme" v-model="draft.theme" class="input">
        <option value="light">浅色</option>
        <option value="dark">深色</option>
        <option value="system">跟随系统</option>
      </select>
    </SettingsRow>
    <SettingsRow label="字体缩放" description="保存后写入 --font-scale，由全局样式消费。" for-id="s-scale">
      <select id="s-scale" v-model.number="draft.fontScale" class="input">
        <option v-for="s in scaleOptions" :key="s" :value="s">{{ Math.round(s * 100) }}%</option>
      </select>
    </SettingsRow>
  </section>
</template>

<style scoped>
.section { padding: 20px 24px; }
</style>
