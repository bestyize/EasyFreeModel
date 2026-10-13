<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import BaseModal from '@/components/BaseModal.vue'
import { useProvidersStore } from '@/stores/providers'
import type { CustomProtocol, ProviderDetail } from '@/types'
import { CUSTOM_PROTOCOLS, validateApiKey, validateCustomProviderForm } from './validation'

/**
 * 创建自定义提供商：名称、协议、Base URL，可选首个 API Key。
 * 格式校验在这里先给出可读提示，最终裁决在 Rust（https 或回环地址 http）。
 * API Key 只存在于下面的本地 ref：提交时立刻清空，不进入 store / 本地存储 / 日志。
 */
const props = withDefaults(defineProps<{ modelValue: boolean; initialProtocol?: CustomProtocol }>(), {
  initialProtocol: 'openai-chat',
})
const emit = defineEmits<{ 'update:modelValue': [value: boolean]; created: [detail: ProviderDetail] }>()

const store = useProvidersStore()
const name = ref('')
const protocol = ref<CustomProtocol>(props.initialProtocol)
const baseUrl = ref('')
const apiKey = ref('')
const submitting = ref(false)
const issues = ref<string[]>([])
const error = ref<string | null>(null)
/** 提供商已创建、但首个 Key 保存失败时记下它：重试只补 Key，不会重复创建提供商 */
const createdDetail = ref<ProviderDetail | null>(null)

function reset() {
  createdDetail.value = null
  name.value = ''
  protocol.value = props.initialProtocol
  baseUrl.value = ''
  apiKey.value = ''
  issues.value = []
  error.value = null
}

watch(
  () => props.modelValue,
  (open) => {
    if (open) reset()
  },
  { immediate: true },
)
onBeforeUnmount(() => {
  apiKey.value = ''
})

const dirty = computed(() => !!(name.value.trim() || baseUrl.value.trim() || apiKey.value))
const placeholder = computed(() =>
  protocol.value === 'anthropic-messages'
    ? 'https://api.example.com'
    : protocol.value === 'gemini'
      ? 'https://generativelanguage.example.com/v1beta'
      : 'https://api.example.com/v1',
)

async function submit() {
  if (submitting.value) return
  const retryKeyOnly = createdDetail.value !== null
  const found = retryKeyOnly
    ? []
    : validateCustomProviderForm({ name: name.value, protocol: protocol.value, baseUrl: baseUrl.value })
  const keyIssue = apiKey.value.trim() ? validateApiKey(apiKey.value) : null
  if (keyIssue) found.push(keyIssue)
  if (retryKeyOnly && !apiKey.value.trim()) found.push('请填写 API Key，或直接关闭（提供商已创建）')
  issues.value = found
  if (found.length) return

  // 取出密钥后立刻清空输入框
  const key = apiKey.value.trim()
  apiKey.value = ''
  error.value = null
  submitting.value = true
  try {
    const created =
      createdDetail.value ??
      (await store.createCustomProvider({
        name: name.value.trim(),
        protocol: protocol.value,
        baseUrl: baseUrl.value.trim(),
      }))
    if (key) {
      try {
        await store.addConnection({ providerId: created.id, accountLabel: '默认账户', apiKey: key })
      } catch (e) {
        createdDetail.value = created
        error.value = `提供商已创建，但保存 API Key 失败：${e instanceof Error ? e.message : String(e)}。可重新输入后再试，或关闭后在详情里添加。`
        emit('created', created)
        return
      }
    }
    emit('created', created)
    emit('update:modelValue', false)
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
  } finally {
    submitting.value = false
  }
}
</script>

<template>
  <BaseModal :model-value="modelValue" title="添加自定义提供商" width="560px" :dirty="dirty && !submitting" @update:model-value="emit('update:modelValue', $event)">
    <form id="custom-provider-form" class="form" @submit.prevent="submit">
      <label class="field">
        <span>名称</span>
        <input v-model="name" class="input" maxlength="64" autocomplete="off" placeholder="例如：公司内部网关" aria-label="提供商名称" :disabled="submitting || createdDetail !== null" />
      </label>
      <label class="field">
        <span>协议</span>
        <select v-model="protocol" class="input" aria-label="协议" :disabled="submitting || createdDetail !== null">
          <option v-for="p in CUSTOM_PROTOCOLS" :key="p.value" :value="p.value">{{ p.label }}</option>
        </select>
      </label>
      <label class="field">
        <span>Base URL</span>
        <input v-model="baseUrl" class="input" autocomplete="off" spellcheck="false" :placeholder="placeholder" aria-label="Base URL" :disabled="submitting || createdDetail !== null" />
        <small>必须是 https；本机服务可用 http://localhost、http://127.0.0.1 或 http://[::1]。</small>
      </label>
      <label class="field">
        <span>API Key（可选）</span>
        <input
          v-model="apiKey"
          class="input"
          type="password"
          autocomplete="new-password"
          spellcheck="false"
          placeholder="稍后也可以在提供商详情里添加"
          aria-label="API Key"
          :disabled="submitting"
        />
        <small>密钥只保存到系统凭据库，界面与数据库都不会保留明文。</small>
      </label>
      <div v-if="issues.length" class="issues" role="alert">
        <p v-for="(it, i) in issues" :key="i">{{ it }}</p>
      </div>
      <p v-if="error" class="issues" role="alert">{{ error }}</p>
    </form>
    <template #footer>
      <button type="button" class="btn" :disabled="submitting" @click="emit('update:modelValue', false)">取消</button>
      <button type="submit" form="custom-provider-form" class="btn btn-primary" :disabled="submitting">
        {{ submitting ? '创建中…' : '创建' }}
      </button>
    </template>
  </BaseModal>
</template>

<style scoped>
.form { display: flex; flex-direction: column; gap: 14px; padding-top: 4px; }
.field { display: flex; flex-direction: column; gap: 6px; font-size: 13px; color: var(--c-text-2); }
.field small { font-size: 12px; }
.issues { font-size: 12px; color: var(--c-danger); }
.issues p { margin: 0; }
</style>
