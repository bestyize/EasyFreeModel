import type { Protocol } from '@/types'

export interface ProtocolOption {
  id: Protocol
  label: string
  /**
   * 后端适配器是否已真实接入。契约目前没有查询接口，这里是前端的单一事实来源；
   * 后端完成对应协议适配器后再改为 true，界面不得伪造“已支持”。
   */
  implemented: boolean
}

export const PROTOCOL_OPTIONS: ProtocolOption[] = [
  { id: 'openai-chat', label: 'OpenAI Chat Completions', implemented: false },
  { id: 'openai-responses', label: 'OpenAI Responses', implemented: false },
  { id: 'anthropic-messages', label: 'Anthropic Messages', implemented: false },
  { id: 'gemini', label: 'Gemini', implemented: false },
]

export function protocolLabel(id: Protocol): string {
  return PROTOCOL_OPTIONS.find((p) => p.id === id)?.label ?? id
}
