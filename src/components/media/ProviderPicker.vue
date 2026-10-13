<script setup lang="ts">
import { useId } from 'vue'
import type { ProviderSummary } from '@/types'

defineProps<{ providers: ProviderSummary[]; providerId: string; model: string; disabled?: boolean }>()
const emit = defineEmits<{ 'update:providerId': [value: string]; 'update:model': [value: string] }>()
const uid = useId()
</script>

<template>
  <div class="picker">
    <div class="field">
      <label :for="`${uid}-p`">供应商</label>
      <select
        :id="`${uid}-p`" class="input" :value="providerId" :disabled="disabled"
        @change="emit('update:providerId', ($event.target as HTMLSelectElement).value)"
      >
        <option v-for="p in providers" :key="p.id" :value="p.id">{{ p.name }}</option>
      </select>
    </div>
    <div class="field">
      <label :for="`${uid}-m`">模型（可选）</label>
      <input
        :id="`${uid}-m`" class="input" :value="model" :disabled="disabled" placeholder="留空使用供应商默认模型"
        autocomplete="off" @input="emit('update:model', ($event.target as HTMLInputElement).value)"
      />
    </div>
  </div>
</template>

<style scoped>
.picker { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
.field { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
label { font-size: var(--fs-small); font-weight: 500; }
</style>
