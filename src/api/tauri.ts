import { invoke } from '@tauri-apps/api/core'
import type { Api } from './types'
import { toOAuthApiError } from './oauthError'

/** OAuth 命令统一把 `{ code, message }` 拒绝转成 OAuthApiError */
async function oauth<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args)
  } catch (e) {
    throw toOAuthApiError(e)
  }
}

export const tauriApi: Api = {
  listProviders: () => invoke('list_providers'),
  getProvider: (id) => invoke('get_provider', { id }),
  saveProvider: (detail) => invoke('save_provider', { detail }),
  testConnection: (connectionId) => invoke('test_connection', { connectionId }),
  createCustomProvider: (input) =>
    invoke('create_custom_provider', { name: input.name, protocol: input.protocol, baseUrl: input.baseUrl }),
  deleteCustomProvider: (id) => invoke('delete_custom_provider', { id }),
  addConnection: (input) =>
    invoke('add_connection', { providerId: input.providerId, accountLabel: input.accountLabel, apiKey: input.apiKey }),
  updateConnectionCredential: (connectionId, apiKey) =>
    invoke('update_connection_credential', { connectionId, apiKey }),
  testProviders: (ids) => invoke('test_providers', { ids }),
  fetchProviderModels: (providerId) => invoke('fetch_provider_models', { providerId }),
  testProviderModel: (providerId, modelId) => invoke('test_provider_model', { providerId, modelId }),

  oauthListProviders: () => oauth('oauth_list_providers'),
  oauthSetClientCredentials: (input) =>
    oauth('oauth_set_client_credentials', {
      providerId: input.providerId,
      clientId: input.clientId,
      clientSecret: input.clientSecret ?? null,
      baseUrl: input.baseUrl ?? null,
    }),
  oauthClearClientCredentials: (providerId) => oauth('oauth_clear_client_credentials', { providerId }),
  oauthStart: (providerId, accountLabel) => oauth('oauth_start', { providerId, accountLabel }),
  oauthPoll: (sessionId) => oauth('oauth_poll', { sessionId }),
  oauthCancel: (sessionId) => oauth('oauth_cancel', { sessionId }),
  oauthReauthorize: (connectionId) => oauth('oauth_reauthorize', { connectionId }),
  oauthDisconnect: (connectionId) => oauth('oauth_disconnect', { connectionId }),

  listApiKeys: () => invoke('list_api_keys'),
  createApiKey: (input) => invoke('create_api_key', { input }),
  rotateApiKey: (id) => invoke('rotate_api_key', { id }),
  setApiKeyDisabled: (id, disabled) => invoke('set_api_key_disabled', { id, disabled }),
  deleteApiKey: (id) => invoke('delete_api_key', { id }),

  usageSummary: (range) => invoke('usage_summary', { range }),
  listUsageRequests: (filter) => invoke('list_usage_requests', { filter }),

  listQuotaAccounts: () => invoke('list_quota_accounts'),
  refreshQuota: (connectionId) => invoke('refresh_quota', { connectionId }),
  setQuotaAccountEnabled: (connectionId, enabled) =>
    invoke('set_quota_account_enabled', { connectionId, enabled }),

  systemStatus: () => invoke('system_status'),
  getSettings: () => invoke('get_settings'),
  saveSettings: (settings) => invoke('save_settings', { settings }),
  startGateway: () => invoke('start_gateway'),
  stopGateway: () => invoke('stop_gateway'),

  listMediaJobs: (kind) => invoke('list_media_jobs', { kind }),
  submitMediaJob: (kind, params) => invoke('submit_media_job', { kind, params }),
  cancelMediaJob: (id) => invoke('cancel_media_job', { id }),
}
