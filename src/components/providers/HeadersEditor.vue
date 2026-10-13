<script setup lang="ts">
import { ref } from 'vue'
import { ChevronDown, ChevronUp, Eye, EyeOff, Plus, SlidersHorizontal, Trash2 } from 'lucide-vue-next'
import type { CustomHeader } from '@/types'

const props = defineProps<{ modelValue: CustomHeader[]; readonly?: boolean }>()
const emit = defineEmits<{ 'update:modelValue': [value: CustomHeader[]] }>()

const open = ref(false)
/** 仅本地界面状态：哪些 secret 行临时显示明文 */
const revealed = ref<Set<number>>(new Set())

function patch(i: number, p: Partial<CustomHeader>) {
  emit('update:modelValue', props.modelValue.map((h, idx) => (idx === i ? { ...h, ...p } : h)))
}
function add() {
  open.value = true
  emit('update:modelValue', [...props.modelValue, { key: '', value: '', secret: false }])
}
function remove(i: number) {
  revealed.value = new Set()
  emit('update:modelValue', props.modelValue.filter((_, idx) => idx !== i))
}
function toggleReveal(i: number) {
  const next = new Set(revealed.value)
  if (next.has(i)) next.delete(i)
  else next.add(i)
  revealed.value = next
}
</script>

<template>
  <section class="panel" aria-label="Custom Headers">
    <button type="button" class="head" :aria-expanded="open" aria-controls="headers-body" @click="open = !open">
      <SlidersHorizontal :size="16" aria-hidden="true" />
      <span class="title">Custom Headers</span>
      <span v-if="modelValue.length" class="count">{{ modelValue.length }}</span>
      <span class="spacer" />
      <component :is="open ? ChevronUp : ChevronDown" :size="16" />
    </button>
    <div v-if="open" id="headers-body" class="body">
      <p v-if="!modelValue.length" class="empty">暂无自定义 Header。</p>
      <ul v-else class="rows">
        <li v-for="(h, i) in modelValue" :key="i" class="row">
          <input
            class="input key"
            :value="h.key"
            :disabled="readonly"
            placeholder="Header 名称"
            :aria-label="`Header 名称 ${i + 1}`"
            @input="patch(i, { key: ($event.target as HTMLInputElement).value })"
          />
          <input
            class="input val"
            :type="h.secret && !revealed.has(i) ? 'password' : 'text'"
            :value="h.value"
            :disabled="readonly"
            autocomplete="off"
            placeholder="值"
            :aria-label="`Header 值 ${i + 1}`"
            @input="patch(i, { value: ($event.target as HTMLInputElement).value })"
          />
          <button
            v-if="h.secret"
            type="button"
            class="icon-btn"
            :aria-label="revealed.has(i) ? `隐藏 Header 值 ${i + 1}` : `显示 Header 值 ${i + 1}`"
            @click="toggleReveal(i)"
          >
            <component :is="revealed.has(i) ? EyeOff : Eye" :size="16" />
          </button>
          <label class="secret">
            <input
              type="checkbox"
              :checked="h.secret"
              :disabled="readonly"
              :aria-label="`Header ${i + 1} 为机密字段`"
              @change="patch(i, { secret: ($event.target as HTMLInputElement).checked })"
            />
            机密
          </label>
          <button type="button" class="icon-btn del" :disabled="readonly" :aria-label="`删除 Header ${i + 1}`" @click="remove(i)">
            <Trash2 :size="16" />
          </button>
        </li>
      </ul>
      <button type="button" class="btn" :disabled="readonly" @click="add"><Plus :size="14" /> 添加 Header</button>
    </div>
  </section>
</template>

<style scoped>
.panel { border: 1px solid var(--c-border); border-radius: var(--radius); background: var(--c-canvas); }
.head { display: flex; align-items: center; gap: 10px; width: 100%; height: 48px; padding: 0 16px; border: 0; background: transparent; font-size: 14px; font-weight: 600; text-align: left; border-radius: var(--radius); }
.head:hover { background: var(--c-sidebar); }
.title { font-weight: 600; }
.count { padding: 0 7px; border-radius: 10px; background: var(--c-surface-2); font-size: 12px; font-weight: 500; color: var(--c-text-2); }
.spacer { flex: 1; }
.body { display: flex; flex-direction: column; gap: 10px; align-items: flex-start; padding: 4px 16px 16px; }
.empty { margin: 0; font-size: 13px; color: var(--c-text-2); }
.rows { display: flex; flex-direction: column; gap: 8px; width: 100%; margin: 0; padding: 0; list-style: none; }
.row { display: flex; align-items: center; gap: 8px; }
.key { flex: 0 0 220px; }
.val { flex: 1; min-width: 0; }
.secret { display: inline-flex; align-items: center; gap: 4px; font-size: 12px; color: var(--c-text-2); white-space: nowrap; }
.secret input { accent-color: var(--c-brand); }
.icon-btn { display: grid; place-items: center; width: 32px; height: 32px; flex: none; border: 0; border-radius: 8px; background: transparent; color: var(--c-text-2); }
.icon-btn:hover:not(:disabled) { background: var(--c-surface-2); }
.icon-btn.del { color: var(--c-danger); }
.icon-btn:disabled { opacity: 0.45; cursor: not-allowed; }
</style>
