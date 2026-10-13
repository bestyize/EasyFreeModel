import { fieldState, type FieldDef, type FieldValue } from './capabilities'
import { formatBytes } from './format'
import { fileExtension, toFilePayload } from './files'

export type FormValues = Record<string, FieldValue | undefined>
export type FormErrors = Record<string, string>

export function defaultValues(fields: FieldDef[]): FormValues {
  const v: FormValues = {}
  for (const f of fields) {
    if (f.type === 'file') v[f.key] = null
    else if (f.type === 'switch') v[f.key] = f.default ?? false
    else v[f.key] = f.default ?? ''
  }
  return v
}

export function isEmpty(value: FieldValue | undefined): boolean {
  return value === undefined || value === null || (typeof value === 'string' && value.trim() === '')
}

/** 校验单个字段，返回错误文本或 null。 */
export function validateField(field: FieldDef, value: FieldValue | undefined): string | null {
  if (isEmpty(value)) {
    if (!field.required) return null
    return field.type === 'file' ? `请选择${field.label}` : field.type === 'select' ? `请选择${field.label}` : `请填写${field.label}`
  }
  switch (field.type) {
    case 'text':
    case 'textarea': {
      const s = String(value)
      if (field.maxLength && s.length > field.maxLength) return `${field.label}不能超过 ${field.maxLength} 个字符（当前 ${s.length}）`
      return null
    }
    case 'number': {
      const n = typeof value === 'number' ? value : Number(value)
      if (!Number.isFinite(n)) return `${field.label}必须是数字`
      if (field.integer && !Number.isInteger(n)) return `${field.label}必须是整数`
      if (field.min !== undefined && n < field.min) return `${field.label}不能小于 ${field.min}`
      if (field.max !== undefined && n > field.max) return `${field.label}不能大于 ${field.max}`
      return null
    }
    case 'select': {
      if (!field.options?.some((o) => o.value === value)) return `${field.label}的取值无效`
      return null
    }
    case 'file': {
      const file = value as File
      if (field.accept && !field.accept.includes(fileExtension(file.name))) {
        return `不支持的文件格式，仅支持：${field.accept.join(' / ')}`
      }
      if (field.maxBytes && file.size > field.maxBytes) {
        return `文件过大（${formatBytes(file.size)}），上限 ${formatBytes(field.maxBytes)}`
      }
      if (file.size === 0) return '文件为空'
      return null
    }
    default:
      return null
  }
}

/** 校验所有可见且启用的字段；被能力表禁用 / 隐藏的字段跳过。 */
export function validateFields(fields: FieldDef[], values: FormValues, providerId?: string | null): FormErrors {
  const errors: FormErrors = {}
  for (const f of fields) {
    if (fieldState(f, providerId) !== 'enabled') continue
    const err = validateField(f, values[f.key])
    if (err) errors[f.key] = err
  }
  return errors
}

/** 构造提交参数：只包含启用且非空的字段，数字转 number，文件转载荷。 */
export async function buildParams(fields: FieldDef[], values: FormValues, providerId?: string | null): Promise<Record<string, unknown>> {
  const params: Record<string, unknown> = {}
  for (const f of fields) {
    if (fieldState(f, providerId) !== 'enabled') continue
    const v = values[f.key]
    if (f.type === 'switch') {
      params[f.key] = v === true
      continue
    }
    if (isEmpty(v)) continue
    if (f.type === 'file') params[f.key] = await toFilePayload(v as File)
    else if (f.type === 'number') params[f.key] = Number(v)
    else if (f.type === 'text' || f.type === 'textarea') params[f.key] = String(v).trim()
    else params[f.key] = v
  }
  return params
}
