import type { CustomProtocol } from '@/types'

export const CUSTOM_PROTOCOLS: { value: CustomProtocol; label: string }[] = [
  { value: 'openai-chat', label: 'OpenAI 兼容（Chat Completions）' },
  { value: 'openai-responses', label: 'OpenAI 兼容（Responses）' },
  { value: 'anthropic-messages', label: 'Anthropic 兼容（Messages）' },
  { value: 'gemini', label: 'Gemini 兼容' },
]

export type UrlCheck = { ok: true; url: string } | { ok: false; message: string }

function isLoopbackHost(hostname: string): boolean {
  const h = hostname.toLowerCase()
  if (h === 'localhost' || h === '[::1]') return true
  const m = /^(\d{1,3})\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.exec(h)
  return !!m && Number(m[1]) === 127
}

/**
 * 与 Rust 侧 `validate_base_url` 同一套规则（Rust 侧才是最终裁决，这里只是提前给出可读提示）：
 * 必须是 https，或回环地址上的 http；不得带账号密码、查询参数或片段。返回去掉尾部 “/” 的规范形式。
 */
export function validateBaseUrl(raw: string): UrlCheck {
  const text = raw.trim()
  if (!text) return { ok: false, message: '请填写 Base URL' }
  if (text.length > 2048) return { ok: false, message: 'Base URL 过长（超过 2048 字符）' }
  let u: URL
  try {
    u = new URL(text)
  } catch {
    return { ok: false, message: 'Base URL 格式无效，应形如 https://api.example.com/v1' }
  }
  if (u.protocol !== 'https:' && u.protocol !== 'http:') {
    return { ok: false, message: 'Base URL 只支持 https（回环地址可用 http）' }
  }
  if (!u.hostname) return { ok: false, message: 'Base URL 缺少主机名' }
  if (u.username || u.password) return { ok: false, message: 'Base URL 不能包含用户名或密码' }
  if (u.search || u.hash) return { ok: false, message: 'Base URL 不能包含查询参数或片段' }
  if (u.protocol === 'http:' && !isLoopbackHost(u.hostname)) {
    return { ok: false, message: 'Base URL 必须使用 https（仅 localhost / 127.0.0.1 / ::1 允许 http）' }
  }
  return { ok: true, url: u.href.replace(/\/+$/, '') }
}

export interface CustomProviderForm {
  name: string
  protocol: CustomProtocol
  baseUrl: string
}

/** 自定义提供商表单校验：返回错误文案列表，空数组表示通过 */
export function validateCustomProviderForm(f: CustomProviderForm): string[] {
  const issues: string[] = []
  const name = f.name.trim()
  if (!name) issues.push('请填写提供商名称')
  else if ([...name].length > 64) issues.push('提供商名称不能超过 64 个字符')
  const url = validateBaseUrl(f.baseUrl)
  if (!url.ok) issues.push(url.message)
  return issues
}

/** API Key 输入校验（错误文案不含密钥内容）。通过返回 null */
export function validateApiKey(key: string): string | null {
  const k = key.trim()
  if (!k) return '请填写 API Key'
  if (k.length > 4096) return 'API Key 过长（超过 4096 字符）'
  if (/\s/.test(k)) return 'API Key 含有空白字符，请检查是否复制完整'
  return null
}

export function validateAccountLabel(label: string): string | null {
  const l = label.trim()
  if (!l) return '请填写名称'
  if ([...l].length > 128) return '名称不能超过 128 个字符'
  return null
}
