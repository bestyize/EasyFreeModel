/**
 * 搜索结果载荷。契约里 MediaJob 没有结构化结果字段，这里约定后端把结果 JSON 放在 job.summary：
 * { origin: 'native' | 'aggregated', provider?: string, results: [{ title, url, snippet? }] }
 * 解析失败时调用方应原样显示文本，不得编造结果。
 */
export interface SearchItem {
  title: string
  url: string
  snippet: string
}

export interface SearchPayload {
  origin: 'native' | 'aggregated' | null
  provider: string | null
  results: SearchItem[]
}

export function safeHttpUrl(raw: unknown): string | null {
  if (typeof raw !== 'string') return null
  try {
    const u = new URL(raw)
    return u.protocol === 'http:' || u.protocol === 'https:' ? u.toString() : null
  } catch {
    return null
  }
}

export function parseSearchPayload(summary: string): SearchPayload | null {
  let data: unknown
  try {
    data = JSON.parse(summary)
  } catch {
    return null
  }
  if (!data || typeof data !== 'object') return null
  const obj = data as Record<string, unknown>
  if (!Array.isArray(obj.results)) return null
  const results: SearchItem[] = []
  for (const r of obj.results) {
    if (!r || typeof r !== 'object') continue
    const item = r as Record<string, unknown>
    const url = safeHttpUrl(item.url)
    if (!url) continue // 非 http(s) 链接（如 javascript:）一律丢弃
    results.push({
      title: typeof item.title === 'string' && item.title ? item.title : url,
      url,
      snippet: typeof item.snippet === 'string' ? item.snippet : '',
    })
  }
  const origin = obj.origin === 'native' || obj.origin === 'aggregated' ? obj.origin : null
  return { origin, provider: typeof obj.provider === 'string' ? obj.provider : null, results }
}
