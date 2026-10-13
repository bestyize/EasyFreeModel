<script setup lang="ts">
import { computed } from 'vue'
import { isLightColor, logoColor, logoText, readableOn } from './status'

/**
 * 纯色块 Logo（无渐变）。优先使用目录给出的品牌色 color 与文字图标 textIcon，
 * 缺失时退回名称哈希色与首字母；浅色底自动换深色字并加细边框，避免白底白字。
 */
const props = withDefaults(
  defineProps<{ name: string; size?: number; color?: string | null; textIcon?: string | null }>(),
  { size: 36, color: null, textIcon: null },
)
const bg = computed(() => props.color || logoColor(props.name))
const text = computed(() => logoText(props.name, props.textIcon))
const style = computed(() => {
  const len = Array.from(text.value).length
  const ratio = len >= 3 ? 0.31 : len === 2 ? 0.38 : 0.44
  return {
    width: `${props.size}px`,
    height: `${props.size}px`,
    background: bg.value,
    color: readableOn(bg.value),
    fontSize: `${Math.round(props.size * ratio)}px`,
    borderRadius: `${Math.round(props.size * 0.25)}px`,
    boxShadow: isLightColor(bg.value) ? 'inset 0 0 0 1px var(--c-border)' : 'none',
  }
})
</script>

<template>
  <span class="logo" :style="style" aria-hidden="true">{{ text }}</span>
</template>

<style scoped>
.logo { display: inline-grid; place-items: center; flex: none; font-weight: 700; letter-spacing: -0.02em; user-select: none; overflow: hidden; }
</style>
