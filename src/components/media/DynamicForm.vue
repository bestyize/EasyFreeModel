<script setup lang="ts">
import { useId } from 'vue'
import ToggleSwitch from '@/components/settings/ToggleSwitch.vue'
import { fieldState, type FieldDef, type FieldValue } from './capabilities'
import type { FormErrors, FormValues } from './forms'
import { formatBytes } from './format'

const props = defineProps<{
  fields: FieldDef[]
  values: FormValues
  errors: FormErrors
  disabled?: boolean
  providerId?: string | null
  /** 不由本组件渲染的字段（例如 ASR 的拖放区自行处理文件） */
  skipKeys?: string[]
}>()
const emit = defineEmits<{ change: [key: string, value: FieldValue] }>()

const uid = useId()
const idOf = (f: FieldDef) => `${uid}-${f.key}`

function visible(f: FieldDef): boolean {
  return !props.skipKeys?.includes(f.key) && fieldState(f, props.providerId) !== 'hidden'
}
function locked(f: FieldDef): boolean {
  return !!props.disabled || fieldState(f, props.providerId) === 'disabled'
}
function fileOf(f: FieldDef): File | null {
  const v = props.values[f.key]
  return v instanceof File ? v : null
}
function len(f: FieldDef): number {
  return String(props.values[f.key] ?? '').length
}

function onInput(f: FieldDef, ev: Event) {
  const el = ev.target as HTMLInputElement
  emit('change', f.key, f.type === 'number' ? (el.value === '' ? '' : Number(el.value)) : el.value)
}
function onSelect(f: FieldDef, ev: Event) {
  const raw = (ev.target as HTMLSelectElement).value
  const opt = f.options?.find((o) => String(o.value) === raw)
  emit('change', f.key, opt ? opt.value : raw)
}
function onFile(f: FieldDef, ev: Event) {
  const el = ev.target as HTMLInputElement
  emit('change', f.key, el.files?.[0] ?? null)
  el.value = '' // 允许重新选择同一文件
}
</script>

<template>
  <div class="dyn">
    <template v-for="f in fields" :key="f.key">
      <div v-if="visible(f)" class="field" :data-field="f.key">
        <template v-if="f.type === 'switch'">
          <div class="switch-row">
            <label :for="idOf(f)">{{ f.label }}</label>
            <ToggleSwitch
              :id="idOf(f)"
              :model-value="values[f.key] === true && fieldState(f, providerId) !== 'disabled'"
              :disabled="locked(f)"
              :label="f.label"
              @update:model-value="emit('change', f.key, $event)"
            />
          </div>
        </template>
        <template v-else>
          <label :for="idOf(f)">{{ f.label }}<span v-if="f.required" class="req" aria-hidden="true"> *</span></label>
          <textarea
            v-if="f.type === 'textarea'"
            :id="idOf(f)" class="input area" rows="5" :value="String(values[f.key] ?? '')"
            :placeholder="f.placeholder" :disabled="locked(f)" :aria-invalid="!!errors[f.key]"
            @input="onInput(f, $event)"
          />
          <select
            v-else-if="f.type === 'select'"
            :id="idOf(f)" class="input" :value="String(values[f.key] ?? '')" :disabled="locked(f)"
            :aria-invalid="!!errors[f.key]" @change="onSelect(f, $event)"
          >
            <option v-for="o in f.options" :key="String(o.value)" :value="String(o.value)">{{ o.label }}</option>
          </select>
          <div v-else-if="f.type === 'file'" class="file">
            <input
              :id="idOf(f)" class="file-input" type="file" :accept="f.accept?.map((e) => '.' + e).join(',')"
              :disabled="locked(f)" :aria-invalid="!!errors[f.key]" @change="onFile(f, $event)"
            />
            <span v-if="fileOf(f)" class="file-name">
              {{ fileOf(f)!.name }}（{{ formatBytes(fileOf(f)!.size) }}）
              <button class="link" type="button" :disabled="locked(f)" @click="emit('change', f.key, null)">移除</button>
            </span>
          </div>
          <input
            v-else
            :id="idOf(f)" class="input" :type="f.type === 'number' ? 'number' : 'text'" :value="String(values[f.key] ?? '')"
            :placeholder="f.placeholder" :min="f.min" :max="f.max" :step="f.step ?? (f.integer ? 1 : 'any')"
            :disabled="locked(f)" :aria-invalid="!!errors[f.key]" autocomplete="off" @input="onInput(f, $event)"
          />
        </template>

        <p v-if="f.type === 'textarea' && f.maxLength" class="counter" :class="{ over: len(f) > f.maxLength }">{{ len(f) }} / {{ f.maxLength }}</p>
        <p v-if="fieldState(f, providerId) === 'disabled'" class="hint">所选供应商未声明支持此选项，已禁用。</p>
        <p v-else-if="f.hint" class="hint">{{ f.hint }}</p>
        <p v-if="errors[f.key]" class="err" role="alert">{{ errors[f.key] }}</p>
      </div>
    </template>
  </div>
</template>

<style scoped>
.dyn { display: flex; flex-direction: column; gap: 14px; }
.field { display: flex; flex-direction: column; gap: 6px; }
label { font-size: var(--fs-small); font-weight: 500; }
.req { color: var(--c-danger); }
.area { height: auto; padding: 10px 12px; resize: vertical; line-height: 1.5; }
.switch-row { display: flex; align-items: center; justify-content: space-between; }
.file { display: flex; flex-direction: column; gap: 6px; font-size: var(--fs-small); }
.file-name { color: var(--c-text-2); }
.link { border: 0; background: transparent; color: var(--c-brand); padding: 0 4px; }
.counter { margin: 0; text-align: right; color: var(--c-text-2); font-size: var(--fs-caption); }
.counter.over { color: var(--c-danger); }
.hint { margin: 0; color: var(--c-text-2); font-size: var(--fs-caption); }
.err { margin: 0; color: var(--c-danger); font-size: var(--fs-caption); }
[aria-invalid='true'] { border-color: var(--c-danger); }
</style>
