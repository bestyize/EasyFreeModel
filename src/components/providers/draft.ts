import type { CustomHeader, ModelEntry, ProviderDetail } from '@/types'

export function cloneDetail(d: ProviderDetail): ProviderDetail {
  return JSON.parse(JSON.stringify(d)) as ProviderDetail
}

/** 去掉完全空白的 header 行，作为 dirty 比较与保存的规范形态 */
export function normalizeHeaders(headers: CustomHeader[]): CustomHeader[] {
  return headers
    .map((h) => ({ key: h.key.trim(), value: h.value, secret: h.secret }))
    .filter((h) => h.key !== '' || h.value !== '')
}

export function normalizeDetail(d: ProviderDetail): ProviderDetail {
  const c = cloneDetail(d)
  c.name = c.name.trim()
  c.customHeaders = normalizeHeaders(c.customHeaders)
  c.models = c.models.map((m) => ({ ...m, publicAlias: m.publicAlias.trim() }))
  return c
}

/** 只比较用户可编辑的部分；未保存修改 = 规范化后与初始快照不同 */
export function isDetailDirty(original: ProviderDetail, draft: ProviderDetail): boolean {
  return JSON.stringify(normalizeDetail(original)) !== JSON.stringify(normalizeDetail(draft))
}

/** 别名冲突：同一提供商内，已启用模型的公开别名不得重复（忽略大小写与首尾空白）。返回 别名(小写) -> 冲突模型 ID 列表 */
export function findAliasConflicts(models: ModelEntry[]): Map<string, string[]> {
  const seen = new Map<string, string[]>()
  for (const m of models) {
    if (!m.enabled) continue
    const key = m.publicAlias.trim().toLowerCase()
    if (!key) continue
    const list = seen.get(key) ?? []
    list.push(m.id)
    seen.set(key, list)
  }
  const out = new Map<string, string[]>()
  for (const [k, ids] of seen) if (ids.length > 1) out.set(k, ids)
  return out
}

export function conflictModelIds(conflicts: Map<string, string[]>): Set<string> {
  return new Set([...conflicts.values()].flat())
}

export interface ValidationIssue {
  field: 'name' | 'alias' | 'headers' | 'distribution' | 'connections'
  message: string
}

/** 保存前校验；任何一项不通过都必须阻止保存并提示，不得静默修正或覆盖 */
export function validateDetail(d: ProviderDetail): ValidationIssue[] {
  const issues: ValidationIssue[] = []
  if (d.group === 'custom' && d.name.trim() === '') {
    issues.push({ field: 'name', message: '请填写提供商名称' })
  }

  const conflicts = findAliasConflicts(d.models)
  for (const [alias, ids] of conflicts) {
    const shown = d.models.find((m) => m.id === ids[0])?.publicAlias.trim() ?? alias
    issues.push({ field: 'alias', message: `模型别名冲突：“${shown}”被 ${ids.length} 个已启用模型使用，请修改后再保存` })
  }
  const emptyAlias = d.models.filter((m) => m.enabled && m.publicAlias.trim() === '')
  if (emptyAlias.length) {
    issues.push({ field: 'alias', message: `有 ${emptyAlias.length} 个已启用模型的公开别名为空` })
  }

  const headers = normalizeHeaders(d.customHeaders)
  if (headers.some((h) => h.key === '')) {
    issues.push({ field: 'headers', message: 'Custom Headers 存在没有名称的行' })
  }
  const keys = headers.map((h) => h.key.toLowerCase())
  const dup = keys.find((k, i) => keys.indexOf(k) !== i)
  if (dup) issues.push({ field: 'headers', message: `Custom Headers 名称重复：${dup}` })

  const { maxConcurrency, maxRetries } = d.distribution
  if (!Number.isInteger(maxConcurrency) || maxConcurrency < 1) {
    issues.push({ field: 'distribution', message: '最大并发需为不小于 1 的整数' })
  }
  if (!Number.isInteger(maxRetries) || maxRetries < 0) {
    issues.push({ field: 'distribution', message: '重试次数需为不小于 0 的整数' })
  }

  if (d.connections.some((c) => c.accountLabel.trim() === '')) {
    issues.push({ field: 'connections', message: '连接的账户名称不能为空' })
  }
  return issues
}

/** 生成保存用数据：规范化并剔除空 header 行 */
export function toSavePayload(d: ProviderDetail): ProviderDetail {
  return normalizeDetail(d)
}
