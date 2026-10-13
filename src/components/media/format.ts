/** 通用格式化与错误文本工具（API 密钥、设置、多模态页面共用）。 */

/** 把 invoke 拒绝值（字符串 / Error / { message }）统一转成可展示文本。 */
export function errorMessage(e: unknown): string {
  if (typeof e === 'string') return e
  if (e instanceof Error) return e.message
  if (e && typeof e === 'object' && 'message' in e && typeof (e as { message: unknown }).message === 'string') {
    return (e as { message: string }).message
  }
  return '未知错误'
}

/** ISO 时间 -> 本地 `YYYY-MM-DD HH:mm`；null / 非法值显示为占位。 */
export function formatDateTime(iso: string | null | undefined, empty = '—'): string {
  if (!iso) return empty
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return empty
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`
}
