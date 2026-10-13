import type { UsageRequest } from '@/types'

export const CSV_COLUMNS = [
  'request_id',
  'provider_id',
  'account_id',
  'model',
  'input_tokens',
  'output_tokens',
  'cached_tokens',
  'latency_ms',
  'status',
  'estimated_cost',
  'timestamp',
  'token_source',
] as const

/**
 * 单元格转义。
 * - 字符串以 = + - @ Tab CR 开头时前置单引号，防止 Excel/Sheets 把它当公式执行（CSV 注入）。
 * - 数字不做注入处理（负数是合法值，且不可能是公式）。
 * - 含逗号、双引号、换行时按 RFC 4180 加引号并转义双引号。
 */
export function escapeCsvCell(value: string | number | null | undefined): string {
  if (value === null || value === undefined) return ''
  let s = typeof value === 'number' ? String(value) : value
  if (typeof value === 'string' && /^[=+\-@\t\r]/.test(s)) s = `'${s}`
  if (/[",\r\n]/.test(s)) s = `"${s.replace(/"/g, '""')}"`
  return s
}

const BOM = '\uFEFF'

export function requestsToCsv(rows: UsageRequest[]): string {
  const lines: string[] = [CSV_COLUMNS.join(',')]
  for (const r of rows) {
    lines.push(
      [
        r.requestId,
        r.providerId,
        r.accountId,
        r.model,
        r.inputTokens,
        r.outputTokens,
        r.cachedTokens,
        r.latencyMs,
        r.status,
        r.estimatedCost,
        r.timestamp,
        r.tokenSource,
      ]
        .map(escapeCsvCell)
        .join(','),
    )
  }
  // BOM 让 Excel 正确识别 UTF-8；CRLF 为 RFC 4180 行尾
  return `${BOM}${lines.join('\r\n')}\r\n`
}

export function downloadCsv(filename: string, csv: string): void {
  const blob = new Blob([csv], { type: 'text/csv;charset=utf-8' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  a.rel = 'noopener'
  document.body.appendChild(a)
  a.click()
  a.remove()
  // 延迟回收，避免部分 WebView 在点击后立即回收导致下载失败
  setTimeout(() => URL.revokeObjectURL(url), 1000)
}
