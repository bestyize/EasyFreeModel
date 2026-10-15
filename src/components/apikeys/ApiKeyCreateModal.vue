<script setup lang="ts">
import { reactive, ref, watch } from 'vue'
import BaseModal from '@/components/BaseModal.vue'
import { errorMessage } from '@/components/media/format'
import { useApiKeysStore } from '@/stores/apiKeys'
import type { ApiKeySecretResult } from '@/types'
import { validateCreateForm, type CreateKeyErrors, type CreateKeyForm } from './validation'

const props = defineProps<{ modelValue: boolean }>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean]; created: [result: ApiKeySecretResult] }>()

const store = useApiKeysStore()

function blank(): CreateKeyForm {
  return { name: '', expiresDate: '', rpm: '', tpm: '', total: '' }
}

const form = reactive<CreateKeyForm>(blank())
const errors = ref<CreateKeyErrors>({})
const submitting = ref(false)
const submitError = ref<string | null>(null)

watch(
  () => props.modelValue,
  () => {
    // 每次打开/关闭都重置，避免残留上一次输入
    Object.assign(form, blank())
    errors.value = {}
    submitError.value = null
    submitting.value = false
  },
)

async function submit() {
  if (submitting.value) return
  const { errors: errs, input } = validateCreateForm(form)
  errors.value = errs
  if (!input) return
  submitting.value = true
  submitError.value = null
  try {
    const result = await store.create(input)
    // secret 只通过事件交给父组件展示，本组件不保留
    emit('created', result)
    emit('update:modelValue', false)
  } catch (e) {
    submitError.value = `创建失败：${errorMessage(e)}`
  } finally {
    submitting.value = false
  }
}

</script>

<template>
  <BaseModal :model-value="modelValue" title="创建密钥" width="600px" @update:model-value="emit('update:modelValue', $event)">
    <form id="create-key-form" class="form" novalidate @submit.prevent="submit">
      <div class="field">
        <label for="ck-name">名称</label>
        <input id="ck-name" v-model="form.name" class="input" maxlength="64" placeholder="例如：Cursor 本地调用" autocomplete="off" />
        <p v-if="errors.name" class="err" role="alert">{{ errors.name }}</p>
      </div>

      <div class="field">
        <label for="ck-exp">过期时间（可选）</label>
        <input id="ck-exp" v-model="form.expiresDate" class="input" type="date" />
        <p v-if="errors.expiresDate" class="err" role="alert">{{ errors.expiresDate }}</p>
        <p v-else class="hint">留空表示永不过期，到期当天 23:59:59 失效。</p>
      </div>

      <p class="hint">密钥支持全部协议和模型，无需配置使用范围。</p>

      <div class="limits">
        <div class="field">
          <label for="ck-rpm">RPM 限制</label>
          <input id="ck-rpm" v-model="form.rpm" class="input" inputmode="numeric" placeholder="不限制" />
          <p v-if="errors.rpm" class="err" role="alert">{{ errors.rpm }}</p>
        </div>
        <div class="field">
          <label for="ck-tpm">TPM 限制</label>
          <input id="ck-tpm" v-model="form.tpm" class="input" inputmode="numeric" placeholder="不限制" />
          <p v-if="errors.tpm" class="err" role="alert">{{ errors.tpm }}</p>
        </div>
        <div class="field">
          <label for="ck-total">累计限额（Token）</label>
          <input id="ck-total" v-model="form.total" class="input" inputmode="numeric" placeholder="不限制" />
          <p v-if="errors.total" class="err" role="alert">{{ errors.total }}</p>
        </div>
      </div>

      <p v-if="submitError" class="err submit-err" role="alert">{{ submitError }}</p>
    </form>
    <template #footer>
      <button class="btn" type="button" @click="emit('update:modelValue', false)">取消</button>
      <button class="btn btn-primary" type="submit" form="create-key-form" data-testid="submit-create" :disabled="submitting">
        {{ submitting ? '创建中…' : '创建' }}
      </button>
    </template>
  </BaseModal>
</template>

<style scoped>
.form { display: flex; flex-direction: column; gap: 16px; }
.field { display: flex; flex-direction: column; gap: 6px; margin: 0; padding: 0; border: 0; min-width: 0; }
label { font-size: var(--fs-small); font-weight: 500; padding: 0; }
.limits { display: grid; grid-template-columns: repeat(3, 1fr); gap: 12px; }
.hint { margin: 0; color: var(--c-text-2); font-size: var(--fs-caption); }
.err { margin: 0; color: var(--c-danger); font-size: var(--fs-caption); }
.submit-err { font-size: var(--fs-small); }
</style>
