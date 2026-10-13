import type { CreateApiKeyInput, Protocol } from '@/types'

export type LimitParse = { ok: true; value: number | null } | { ok: false; error: string }

/** 可选正整数：空 -> null；必须是十进制正整数且在安全整数范围内。 */
export function parseOptionalPositiveInt(raw: string | number | null | undefined, label: string): LimitParse {
  const s = String(raw ?? '').trim()
  if (s === '') return { ok: true, value: null }
  if (!/^\d+$/.test(s)) return { ok: false, error: `${label}必须是正整数` }
  const n = Number(s)
  if (!Number.isSafeInteger(n) || n <= 0) return { ok: false, error: `${label}必须是大于 0 的整数` }
  return { ok: true, value: n }
}

export interface CreateKeyForm {
  name: string
  /** yyyy-mm-dd，空表示永不过期 */
  expiresDate: string
  protocols: Protocol[]
  /** 以换行 / 逗号 / 空白分隔，空表示不限制模型 */
  modelsText: string
  rpm: string
  tpm: string
  total: string
}

export type CreateKeyErrors = Partial<Record<'name' | 'expiresDate' | 'protocols' | 'rpm' | 'tpm' | 'total', string>>

export function parseModels(text: string): string[] {
  return [...new Set(text.split(/[\s,，;；]+/).map((m) => m.trim()).filter(Boolean))]
}

export function validateCreateForm(
  form: CreateKeyForm,
  now: Date = new Date(),
): { errors: CreateKeyErrors; input: CreateApiKeyInput | null } {
  const errors: CreateKeyErrors = {}

  const name = form.name.trim()
  if (!name) errors.name = '请输入名称'
  else if (name.length > 64) errors.name = '名称不能超过 64 个字符'

  let expiresAt: string | null = null
  if (form.expiresDate) {
    const m = form.expiresDate.match(/^(\d{4})-(\d{2})-(\d{2})$/)
    if (!m) errors.expiresDate = '日期格式不正确'
    else {
      // 当天 23:59:59（本地时间）到期
      const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]), 23, 59, 59)
      if (Number.isNaN(d.getTime())) errors.expiresDate = '日期格式不正确'
      else if (d.getTime() <= now.getTime()) errors.expiresDate = '过期时间必须晚于当前时间'
      else expiresAt = d.toISOString()
    }
  }

  if (form.protocols.length === 0) errors.protocols = '至少选择一种协议'

  const rpm = parseOptionalPositiveInt(form.rpm, 'RPM')
  const tpm = parseOptionalPositiveInt(form.tpm, 'TPM')
  const total = parseOptionalPositiveInt(form.total, '累计限额')
  if (!rpm.ok) errors.rpm = rpm.error
  if (!tpm.ok) errors.tpm = tpm.error
  if (!total.ok) errors.total = total.error

  if (Object.keys(errors).length > 0 || !rpm.ok || !tpm.ok || !total.ok) return { errors, input: null }

  return {
    errors,
    input: {
      name,
      expiresAt,
      protocols: [...form.protocols],
      allowedModels: parseModels(form.modelsText),
      rpmLimit: rpm.value,
      tpmLimit: tpm.value,
      totalLimit: total.value,
    },
  }
}
