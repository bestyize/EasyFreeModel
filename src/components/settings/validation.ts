import type { AppSettings } from '@/types'
import { isValidListenAddress } from './address'

export type SettingsErrors = Partial<Record<'listenAddress' | 'port' | 'timeoutSeconds' | 'retryCount', string>>

function intInRange(v: unknown, min: number, max: number): boolean {
  return typeof v === 'number' && Number.isInteger(v) && v >= min && v <= max
}

export function validateSettings(s: AppSettings): SettingsErrors {
  const e: SettingsErrors = {}
  if (!isValidListenAddress(String(s.listenAddress ?? ''))) e.listenAddress = '请输入有效的 IPv4 / IPv6 地址或 localhost'
  if (!intInRange(s.port, 1, 65535)) e.port = '端口必须是 1–65535 的整数'
  if (!intInRange(s.timeoutSeconds, 1, 600)) e.timeoutSeconds = '超时必须是 1–600 秒的整数'
  if (!intInRange(s.retryCount, 0, 10)) e.retryCount = '重试次数必须是 0–10 的整数'
  return e
}
