<script setup lang="ts">
import { computed } from 'vue'
import { colorFor } from './format'

const props = withDefaults(defineProps<{ id: string; name: string; size?: number }>(), { size: 24 })

const letter = computed(() => (props.name.trim()[0] ?? props.id[0] ?? '?').toUpperCase())
const style = computed(() => ({
  width: `${props.size}px`,
  height: `${props.size}px`,
  fontSize: `${Math.round(props.size * 0.5)}px`,
  background: colorFor(props.id),
}))
</script>

<template>
  <span class="avatar" :style="style" aria-hidden="true">{{ letter }}</span>
</template>

<style scoped>
.avatar {
  flex: none;
  display: inline-grid;
  place-items: center;
  border-radius: 7px;
  color: #fff;
  font-weight: 600;
  line-height: 1;
}
</style>
