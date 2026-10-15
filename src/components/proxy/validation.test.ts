import { describe, expect, it } from 'vitest'
import type { ProxyNode } from '@/types'
import { nodeAddress, statusOf } from './format'
import { blankNodeForm, formFromNode, validateNodeForm } from './validation'

const form = (over: Partial<ReturnType<typeof blankNodeForm>> = {}) => ({ ...blankNodeForm(), host: 'proxy.example.com', port: '8080', ...over })

describe('validateNodeForm', () => {
  it('名称留空时用 主机:端口', () => {
    const { input, errors } = validateNodeForm(form())
    expect(errors).toEqual({})
    expect(input).toMatchObject({ name: 'proxy.example.com:8080', scheme: 'http', host: 'proxy.example.com', port: 8080, username: null, password: '' })
    expect(input).not.toHaveProperty('id')
  })

  it('拒绝无效主机与端口', () => {
    for (const host of ['', 'http://x', 'a b', 'u@x', 'x/y']) expect(validateNodeForm(form({ host })).errors.host).toBeTruthy()
    for (const port of ['', '0', '65536', '1.5', 'abc']) expect(validateNodeForm(form({ port })).errors.port).toBeTruthy()
    expect(validateNodeForm(form({ name: 'x'.repeat(65) })).errors.name).toBeTruthy()
  })

  it('密码语义：有用户名且填写 = 覆盖；留空 = 保持(undefined)；无用户名/勾选清除 = 空串', () => {
    expect(validateNodeForm(form({ username: 'a', password: 'pw' }), 'n1').input).toMatchObject({ id: 'n1', username: 'a', password: 'pw' })
    expect(validateNodeForm(form({ username: 'a' }), 'n1').input!.password).toBeUndefined()
    expect(validateNodeForm(form({ username: 'a', password: 'pw', clearPassword: true })).input!.password).toBe('')
    expect(validateNodeForm(form({ username: '  ', password: 'pw' })).input!.password).toBe('')
  })

  it('IPv6 方括号会被去掉', () => {
    expect(validateNodeForm(form({ host: '[::1]' })).input!.host).toBe('::1')
  })
})

const node = (over: Partial<ProxyNode> = {}): ProxyNode => ({
  id: 'n1', name: 'n', scheme: 'socks5', host: '::1', port: 1080, username: null, hasPassword: false, enabled: true,
  status: 'healthy', latencyMs: null, lastCheckedAt: null, lastError: null, exitIp: null, successCount: 0, failureCount: 0,
  cooldownRemainingSeconds: 0, ...over,
})

describe('format', () => {
  it('地址展示不含账号并为 IPv6 加方括号', () => {
    expect(nodeAddress(node())).toBe('socks5://[::1]:1080')
    expect(nodeAddress(node({ host: 'a.io', scheme: 'http', port: 80 }))).toBe('http://a.io:80')
  })

  it('状态标签：停用优先于健康状态', () => {
    expect(statusOf(node({ enabled: false }))).toEqual({ label: '已停用', tone: 'neutral' })
    expect(statusOf(node({ status: 'cooldown' })).tone).toBe('warning')
    expect(statusOf(node({ status: 'unhealthy' })).tone).toBe('danger')
  })

  it('formFromNode 不带密码', () => {
    expect(formFromNode(node({ hasPassword: true, username: 'u' })).password).toBe('')
  })
})
