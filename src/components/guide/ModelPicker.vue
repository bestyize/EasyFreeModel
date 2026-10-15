<script setup lang="ts">
import { computed, ref } from 'vue'
import type { GatewayModel } from '@/types'

const props = withDefaults(defineProps<{
  id: string
  label: string
  modelValue: string
  options: GatewayModel[]
  optional?: boolean
}>(), { optional: false })
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()
const manual = ref(false)
const custom = '__freeapibox_custom_model__'

const groups = computed(() => {
  const map = new Map<string, GatewayModel[]>()
  for (const model of props.options) {
    const name = model.providerName || model.providerId
    const group = map.get(name) ?? []
    group.push(model)
    map.set(name, group)
  }
  return [...map.entries()]
})
const isCustom = computed(() => manual.value || (!!props.modelValue && !props.options.some((m) => m.id === props.modelValue)))
const selected = computed(() => isCustom.value ? custom : props.modelValue)

function select(e: Event) {
  const value = (e.target as HTMLSelectElement).value
  manual.value = value === custom
  emit('update:modelValue', manual.value ? '' : value)
}
</script>

<template>
  <div class="field">
    <label :for="id">{{ label }}</label>
    <select v-if="options.length" :id="id" :value="selected" :aria-label="label" @change="select">
      <option value="">{{ optional ? '沿用默认模型' : '请选择模型' }}</option>
      <optgroup v-for="[provider, models] in groups" :key="provider" :label="provider">
        <option v-for="entry in models" :key="entry.providerId + '/' + entry.id" :value="entry.id">{{ entry.id }}</option>
      </optgroup>
      <option :value="custom">手动输入模型名…</option>
    </select>
    <input
      v-if="isCustom || !options.length"
      :id="options.length ? id + '-custom' : id"
      :value="modelValue"
      :placeholder="optional ? '留空沿用默认模型' : '输入网关模型名'"
      autocomplete="off" spellcheck="false"
      @input="emit('update:modelValue', ($event.target as HTMLInputElement).value)"
    />
  </div>
</template>

<style scoped>
.field { display: flex; flex-direction: column; gap: 6px; min-width: 0; font-size: var(--fs-small); }
label { font-weight: 500; }
select,input { width: 100%; min-width: 0; padding: 9px 10px; border: 1px solid var(--c-border); border-radius: 8px; background: var(--c-canvas); color: var(--c-text); }
</style>
