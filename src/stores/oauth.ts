import { defineStore } from 'pinia'
import { ref } from 'vue'
import { api } from '@/api'
import { oauthErrorText, toOAuthApiError } from '@/api/oauthError'
import type {
  OAuthClientCredentialsInput,
  OAuthPollResult,
  OAuthProviderInfo,
  OAuthStartResult,
} from '@/types'

/**
 * OAuth 授权 store。红线：这里的状态只有提供商能力信息（布尔值/枚举/说明文案）；
 * 令牌、授权码、code_verifier、client secret 不在任何字段里。setClientCredentials 的入参只是透传，
 * 授权完成后需要刷新“提供商列表” store 的由调用方（面板/弹窗）负责。
 */
export const useOAuthStore = defineStore('oauth', () => {
  const providers = ref<OAuthProviderInfo[]>([])
  const loading = ref(false)
  const loaded = ref(false)
  /** 中文错误文案（来自错误码映射，不含服务端原始文本） */
  const error = ref<string | null>(null)

  function infoFor(providerId: string): OAuthProviderInfo | undefined {
    return providers.value.find((p) => p.providerId === providerId)
  }

  async function fetchProviders(): Promise<void> {
    loading.value = true
    error.value = null
    try {
      providers.value = await api.oauthListProviders()
      loaded.value = true
    } catch (e) {
      error.value = oauthErrorText(toOAuthApiError(e).code)
    } finally {
      loading.value = false
    }
  }

  function replaceInfo(info: OAuthProviderInfo) {
    const i = providers.value.findIndex((p) => p.providerId === info.providerId)
    if (i >= 0) providers.value = providers.value.map((p, idx) => (idx === i ? info : p))
    else providers.value = [...providers.value, info]
  }

  /** 保存客户端凭据：入参只透传给后端，store 不保留；用返回的（只含布尔值的）信息替换列表项 */
  async function setClientCredentials(input: OAuthClientCredentialsInput): Promise<OAuthProviderInfo> {
    const info = await api.oauthSetClientCredentials(input)
    replaceInfo(info)
    return info
  }

  async function clearClientCredentials(providerId: string): Promise<OAuthProviderInfo> {
    const info = await api.oauthClearClientCredentials(providerId)
    replaceInfo(info)
    return info
  }

  function start(providerId: string, accountLabel: string): Promise<OAuthStartResult> {
    return api.oauthStart(providerId, accountLabel)
  }

  function reauthorize(connectionId: string): Promise<OAuthStartResult> {
    return api.oauthReauthorize(connectionId)
  }

  function poll(sessionId: string): Promise<OAuthPollResult> {
    return api.oauthPoll(sessionId)
  }

  function submitCallback(sessionId: string, callbackUrl: string): Promise<void> {
    return api.oauthSubmitCallback(sessionId, callbackUrl)
  }

  function cancel(sessionId: string): Promise<void> {
    return api.oauthCancel(sessionId)
  }

  function disconnect(connectionId: string): Promise<void> {
    return api.oauthDisconnect(connectionId)
  }

  return {
    providers,
    loading,
    loaded,
    error,
    infoFor,
    fetchProviders,
    setClientCredentials,
    clearClientCredentials,
    start,
    reauthorize,
    poll,
    submitCallback,
    cancel,
    disconnect,
  }
})
