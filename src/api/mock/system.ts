import type { SystemApi } from '../types'
import type { AppSettings, GatewayStatus } from '@/types'

/** 浏览器预览用内存 mock：用于演示启停与端口占用错误，不代表真实网关。 */
let settings: AppSettings = {
  listenAddress: '127.0.0.1',
  port: 20128,
  autoStart: false,
  trayEnabled: true,
  timeoutSeconds: 120,
  retryCount: 2,
  language: 'zh-CN',
  theme: 'light',
  fontScale: 1,
  redactLogs: true,
}
let running = true
let error: string | null = null

function gateway(): GatewayStatus {
  return { running, address: settings.listenAddress, port: settings.port, error }
}

export const systemMock: SystemApi = {
  checkForUpdate: async () => null,
  systemStatus: async () => ({ gateway: gateway(), apiRequests: 0, successRate: null, avgLatencyMs: null }),
  getSettings: async () => ({ ...settings }),
  saveSettings: async (s) => {
    settings = { ...s }
    return { ...settings }
  },
  startGateway: async () => {
    if (settings.port < 1024) {
      running = false
      error = `端口 ${settings.port} 无法绑定：已被占用或需要管理员权限`
    } else {
      running = true
      error = null
    }
    return gateway()
  },
  listGatewayModels: async () => [
    { id: 'deepseek-chat', providerId: 'deepseek', providerName: 'DeepSeek' },
    { id: 'openai/gpt-4o-mini', providerId: 'openai', providerName: 'OpenAI' },
    { id: 'anthropic/claude-sonnet', providerId: 'anthropic', providerName: 'Anthropic' },
  ],
  stopGateway: async () => {
    running = false
    error = null
    return gateway()
  },
}
