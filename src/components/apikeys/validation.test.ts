import { describe, expect, it } from 'vitest'
import { parseOptionalPositiveInt, validateCreateForm, type CreateKeyForm } from './validation'

function form(over: Partial<CreateKeyForm> = {}): CreateKeyForm {
  return { name: 'dev', expiresDate: '', rpm: '', tpm: '', total: '', ...over }
}

describe('parseOptionalPositiveInt', () => {
  it('空值视为未设置', () => {
    expect(parseOptionalPositiveInt('', 'RPM')).toEqual({ ok: true, value: null })
    expect(parseOptionalPositiveInt('  ', 'RPM')).toEqual({ ok: true, value: null })
  })
  it('接受正整数', () => {
    expect(parseOptionalPositiveInt('60', 'RPM')).toEqual({ ok: true, value: 60 })
  })
  it.each(['0', '-5', '1.5', 'abc', '1e3', '12 3', '9007199254740993'])('拒绝非法值 %s', (raw) => {
    expect(parseOptionalPositiveInt(raw, 'RPM').ok).toBe(false)
  })
})

describe('validateCreateForm', () => {
  it('名称必填', () => {
    const r = validateCreateForm(form({ name: '  ' }))
    expect(r.input).toBeNull()
    expect(r.errors.name).toBeTruthy()
  })
  it('RPM / TPM / 累计限额逐项校验', () => {
    const r = validateCreateForm(form({ rpm: '0', tpm: 'x', total: '1.2' }))
    expect(r.input).toBeNull()
    expect(Object.keys(r.errors).sort()).toEqual(['rpm', 'total', 'tpm'])
  })
  it('过期日期必须在未来', () => {
    const now = new Date(2026, 9, 14, 12, 0, 0)
    expect(validateCreateForm(form({ expiresDate: '2026-10-13' }), now).errors.expiresDate).toBeTruthy()
    const ok = validateCreateForm(form({ expiresDate: '2026-10-15' }), now)
    expect(ok.errors.expiresDate).toBeUndefined()
    expect(ok.input?.expiresAt).toBeTruthy()
  })
  it('合法输入产出请求体', () => {
    const r = validateCreateForm(form({ rpm: '60' }))
    expect(r.input).toMatchObject({ name: 'dev', rpmLimit: 60, tpmLimit: null, totalLimit: null,
      protocols: ['openai-chat', 'openai-responses', 'anthropic-messages', 'gemini'], allowedModels: [] })
  })
})
