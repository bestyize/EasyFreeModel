import type { ProxyNode, ProxyNodeInput, ProxyScheme } from '@/types'

export interface NodeForm {
  name: string
  scheme: ProxyScheme
  host: string
  port: string
  username: string
  password: string
  clearPassword: boolean
  enabled: boolean
}

export type NodeFormErrors = Partial<Record<'name' | 'host' | 'port', string>>

export function blankNodeForm(): NodeForm {
  return { name: '', scheme: 'http', host: '', port: '', username: '', password: '', clearPassword: false, enabled: true }
}

export function formFromNode(n: ProxyNode): NodeForm {
  return {
    name: n.name, scheme: n.scheme, host: n.host, port: String(n.port), username: n.username ?? '', password: '',
    clearPassword: false, enabled: n.enabled,
  }
}

const HOST_BAD = /[\s/\\@?#]/

export function validateNodeForm(form: NodeForm, id?: string): { errors: NodeFormErrors; input: ProxyNodeInput | null } {
  const errors: NodeFormErrors = {}
  const host = form.host.trim().replace(/^\[|\]$/g, '')
  if (!host) errors.host = '请填写代理主机'
  else if (host.length > 253 || HOST_BAD.test(host)) errors.host = '只填写域名或 IP，不要带协议头、路径或账号'
  const port = Number(form.port)
  if (!form.port.trim()) errors.port = '请填写端口'
  else if (!Number.isInteger(port) || port < 1 || port > 65535) errors.port = '端口需在 1–65535 之间'
  const name = form.name.trim() || (host && !errors.port && form.port.trim() ? `${host}:${port}` : '')
  if (!name) errors.name = '请填写名称'
  else if ([...name].length > 64) errors.name = '名称不超过 64 个字符'
  if (Object.keys(errors).length) return { errors, input: null }

  const username = form.username.trim()
  // 密码：undefined 保持；空串清除；有值覆盖。没有用户名就没有密码
  let password: string | undefined
  if (!username || form.clearPassword) password = ''
  else if (form.password) password = form.password
  return {
    errors,
    input: { ...(id ? { id } : {}), name, scheme: form.scheme, host, port, username: username || null, password, enabled: form.enabled },
  }
}
