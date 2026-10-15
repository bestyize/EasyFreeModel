<script setup lang="ts">
import { reactive, ref, watch } from 'vue'
import BaseModal from '@/components/BaseModal.vue'
import { errorMessage } from '@/components/media/format'
import { useProxyPoolStore } from '@/stores/proxyPool'
import type { ProxyNode } from '@/types'
import { SCHEME_OPTIONS } from './format'
import { blankNodeForm, formFromNode, validateNodeForm, type NodeFormErrors } from './validation'

const props = defineProps<{ modelValue: boolean; node: ProxyNode | null }>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()

const store = useProxyPoolStore()
const form = reactive(blankNodeForm())
const errors = ref<NodeFormErrors>({})
const submitting = ref(false)
const submitError = ref<string | null>(null)

watch(
  () => props.modelValue,
  (open) => {
    if (!open) return
    Object.assign(form, props.node ? formFromNode(props.node) : blankNodeForm())
    errors.value = {}
    submitError.value = null
    submitting.value = false
  },
)

async function submit() {
  if (submitting.value) return
  const { errors: errs, input } = validateNodeForm(form, props.node?.id)
  errors.value = errs
  if (!input) return
  submitting.value = true
  submitError.value = null
  try {
    await store.saveNode(input)
    emit('update:modelValue', false)
  } catch (e) {
    submitError.value = `保存失败：${errorMessage(e)}`
  } finally {
    submitting.value = false
  }
}
</script>

<template>
  <BaseModal :model-value="modelValue" :title="node ? '编辑代理节点' : '添加代理节点'" width="560px" @update:model-value="emit('update:modelValue', $event)">
    <form id="proxy-node-form" class="form" novalidate @submit.prevent="submit">
      <div class="field">
        <label for="px-name">名称（可选）</label>
        <input id="px-name" v-model="form.name" class="input" maxlength="64" placeholder="留空则使用 主机:端口" autocomplete="off" />
        <p v-if="errors.name" class="err" role="alert">{{ errors.name }}</p>
      </div>

      <div class="row">
        <div class="field scheme">
          <label for="px-scheme">协议</label>
          <select id="px-scheme" v-model="form.scheme" class="input">
            <option v-for="s in SCHEME_OPTIONS" :key="s.id" :value="s.id">{{ s.label }}</option>
          </select>
        </div>
        <div class="field grow">
          <label for="px-host">主机</label>
          <input id="px-host" v-model="form.host" class="input" placeholder="例如 proxy.example.com 或 10.0.0.2" autocomplete="off" spellcheck="false" />
          <p v-if="errors.host" class="err" role="alert">{{ errors.host }}</p>
        </div>
        <div class="field port">
          <label for="px-port">端口</label>
          <input id="px-port" v-model="form.port" class="input" inputmode="numeric" placeholder="1080" autocomplete="off" />
          <p v-if="errors.port" class="err" role="alert">{{ errors.port }}</p>
        </div>
      </div>

      <div class="row">
        <div class="field grow">
          <label for="px-user">用户名（可选）</label>
          <input id="px-user" v-model="form.username" class="input" autocomplete="off" spellcheck="false" />
        </div>
        <div class="field grow">
          <label for="px-pass">密码（可选）</label>
          <input
            id="px-pass" v-model="form.password" class="input" type="password" autocomplete="new-password"
            :placeholder="node?.hasPassword && !form.clearPassword ? '已保存，留空保持不变' : ''"
            :disabled="!form.username.trim()"
          />
        </div>
      </div>
      <label v-if="node?.hasPassword" class="check">
        <input v-model="form.clearPassword" type="checkbox" /> 清除已保存的密码
      </label>
      <p class="hint">密码只保存在本机凭据库，不会显示在界面上，也不会写入数据库。</p>

      <label class="check"><input v-model="form.enabled" type="checkbox" /> 启用此节点</label>

      <p v-if="submitError" class="err submit-err" role="alert">{{ submitError }}</p>
    </form>
    <template #footer>
      <button class="btn" type="button" @click="emit('update:modelValue', false)">取消</button>
      <button class="btn btn-primary" type="submit" form="proxy-node-form" data-testid="submit-node" :disabled="submitting">
        {{ submitting ? '保存中…' : '保存' }}
      </button>
    </template>
  </BaseModal>
</template>

<style scoped>
.form { display: flex; flex-direction: column; gap: 14px; }
.row { display: flex; gap: 12px; align-items: flex-start; }
.field { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
.grow { flex: 1; }
.scheme { width: 150px; flex: none; }
.port { width: 100px; flex: none; }
label { font-size: var(--fs-small); font-weight: 500; }
.check { display: flex; align-items: center; gap: 8px; font-weight: 400; }
.hint { margin: 0; color: var(--c-text-2); font-size: var(--fs-caption); }
.err { margin: 0; color: var(--c-danger); font-size: var(--fs-caption); }
.submit-err { font-size: var(--fs-small); }
</style>
