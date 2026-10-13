<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue'
import { oauthErrorText, toOAuthApiError } from '@/api/oauthError'
import { useOAuthStore } from '@/stores/oauth'
import type { OAuthProviderInfo } from '@/types'

/**
 * 「配置客户端凭据」表单。
 * 红线：clientSecret 只存在于这里的本地 ref。提交时读出后在同一个 tick（await 之前）清空，
 * 只作为 oauthSetClientCredentials 这一次调用的入参；取消、卸载时同样清空。不进 store、不打日志。
 */
const props = defineProps<{ info: OAuthProviderInfo; providerName: string }>()
const emit = defineEmits<{ saved: [info: OAuthProviderInfo]; cleared: [info: OAuthProviderInfo]; cancel: [] }>()

const store = useOAuthStore()

const clientId = ref('')
const secret = ref('')
const baseUrl = ref('')
const busy = ref(false)
const error = ref<string | null>(null)

const secretLabel = computed(() => (props.info.clientSecret === 'required' ? '客户端密钥（必填）' : '客户端密钥（可选）'))

function wipe() {
  secret.value = ''
}
onBeforeUnmount(wipe)

async function submit() {
  if (busy.value) return
  const id = clientId.value.trim()
  if (props.info.clientIdRequired && !id) {
    error.value = '请填写客户端 ID'
    return
  }
  if (props.info.clientSecret === 'required' && !secret.value.trim()) {
    error.value = '请填写客户端密钥'
    return
  }
  const hadSecret = props.info.clientSecret !== 'none' && secret.value !== ''
  const clientSecret = hadSecret ? secret.value : undefined
  wipe() // 先清空输入框，再调用 API
  error.value = null
  busy.value = true
  try {
    const next = await store.setClientCredentials({
      providerId: props.info.providerId,
      clientId: id,
      ...(clientSecret !== undefined ? { clientSecret } : {}),
      ...(props.info.supportsBaseUrl && baseUrl.value.trim() ? { baseUrl: baseUrl.value.trim() } : {}),
    })
    emit('saved', next)
  } catch (e) {
    error.value = oauthErrorText(toOAuthApiError(e).code) + (hadSecret ? '（密钥输入框已清空，请重新输入）' : '')
  } finally {
    busy.value = false
  }
}

async function clearConfigured() {
  if (busy.value) return
  busy.value = true
  error.value = null
  try {
    const next = await store.clearClientCredentials(props.info.providerId)
    emit('cleared', next)
  } catch (e) {
    error.value = oauthErrorText(toOAuthApiError(e).code)
  } finally {
    busy.value = false
  }
}

function cancel() {
  wipe()
  emit('cancel')
}
</script>

<template>
  <form class="cred-form" :aria-label="`配置 ${providerName} 客户端凭据`" @submit.prevent="submit">
    <h4>配置客户端凭据</h4>
    <p class="lead" role="note">需要配置客户端凭据后才能授权。{{ providerName }} 没有内置可用的客户端，请填写你自己注册的应用凭据。</p>

    <label v-if="info.clientIdRequired" class="field">
      <span>客户端 ID</span>
      <input
        v-model="clientId"
        class="input"
        autocomplete="off"
        spellcheck="false"
        maxlength="512"
        placeholder="粘贴客户端 ID"
        aria-label="客户端 ID"
        :disabled="busy"
      />
    </label>

    <label v-if="info.clientSecret !== 'none'" class="field">
      <span>{{ secretLabel }}</span>
      <input
        v-model="secret"
        class="input"
        type="password"
        autocomplete="new-password"
        spellcheck="false"
        placeholder="粘贴客户端密钥"
        aria-label="客户端密钥"
        :disabled="busy"
      />
    </label>

    <label v-if="info.supportsBaseUrl" class="field">
      <span>基础地址（自托管实例，可选）</span>
      <input
        v-model="baseUrl"
        class="input"
        autocomplete="off"
        spellcheck="false"
        placeholder="留空则使用默认地址，例如 https://gitlab.example.com"
        aria-label="基础地址"
        :disabled="busy"
      />
    </label>

    <p class="hint">客户端密钥只保存到系统凭据库，界面、数据库与日志都不会保留明文。</p>
    <p v-if="error" class="err" role="alert">{{ error }}</p>

    <div class="actions">
      <button v-if="info.clientConfigured" type="button" class="btn btn-danger" :disabled="busy" @click="clearConfigured">
        清除客户端凭据
      </button>
      <span class="spacer" />
      <button type="button" class="btn" :disabled="busy" @click="cancel">取消</button>
      <button type="submit" class="btn btn-primary" :disabled="busy">{{ busy ? '保存中…' : '保存并继续' }}</button>
    </div>
  </form>
</template>

<style scoped>
.cred-form { display: flex; flex-direction: column; gap: 10px; }
h4 { margin: 0; font-size: 14px; font-weight: 600; }
.lead { margin: 0; font-size: 13px; color: var(--c-text-2); }
.field { display: flex; flex-direction: column; gap: 4px; font-size: 12px; color: var(--c-text-2); }
.hint { margin: 0; font-size: 12px; color: var(--c-text-2); }
.err { margin: 0; font-size: 12px; color: var(--c-danger); }
.actions { display: flex; align-items: center; gap: 8px; }
.spacer { flex: 1; }
</style>
