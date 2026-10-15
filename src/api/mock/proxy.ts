import type { ProxyApi } from '../types'
import type { ProxyNode, ProxyNodeInput, ProxyPoolConfig, ProxyPoolState, ProxyScheme } from '@/types'

/** 浏览器预览用内存 mock：测速结果按主机名伪造（含 dead 的主机失败），不代表真实网络。 */
let config: ProxyPoolConfig = {
  enabled: false,
  strategy: 'roundRobin',
  providerScope: 'all',
  providerIds: [],
  failureThreshold: 3,
  cooldownSeconds: 60,
  maxAttempts: 2,
  fallbackDirect: false,
  testUrl: 'https://www.cloudflare.com/cdn-cgi/trace',
}
let nodes: ProxyNode[] = []
let seq = 0

const SCHEMES: ProxyScheme[] = ['http', 'https', 'socks5', 'socks5h']

function state(): ProxyPoolState {
  return { config: { ...config, providerIds: [...config.providerIds] }, nodes: nodes.map((n) => ({ ...n })) }
}

function blankNode(input: ProxyNodeInput, id: string): ProxyNode {
  return {
    id,
    name: input.name.trim(),
    scheme: input.scheme,
    host: input.host.trim().toLowerCase(),
    port: input.port,
    username: input.username?.trim() || null,
    hasPassword: !!input.username?.trim() && !!input.password,
    enabled: input.enabled,
    status: 'untested',
    latencyMs: null,
    lastCheckedAt: null,
    lastError: null,
    exitIp: null,
    successCount: 0,
    failureCount: 0,
    cooldownRemainingSeconds: 0,
  }
}

function parseLine(line: string): ProxyNodeInput | string {
  const m = /^(?:(\w+):\/\/)?(?:([^:@/]+):([^@/]*)@)?([^:/@]+):(\d{1,5})$/.exec(line)
  if (!m) return '格式无法识别'
  const scheme = (m[1] ?? 'http').toLowerCase() as ProxyScheme
  if (!SCHEMES.includes(scheme)) return `不支持的协议：${m[1]}`
  const port = Number(m[5])
  if (port < 1 || port > 65535) return '端口无效'
  return { name: `${m[4]}:${port}`, scheme, host: m[4], port, username: m[2] ?? null, password: m[3], enabled: true }
}

function fakeTest(n: ProxyNode): ProxyNode {
  const ok = !n.host.includes('dead')
  const now = new Date().toISOString()
  if (!ok) {
    return {
      ...n, status: 'unhealthy', latencyMs: null, exitIp: null, lastCheckedAt: now, failureCount: n.failureCount + 1,
      lastError: '无法连接到代理，或代理拒绝了连接（请检查地址、端口与认证信息）',
    }
  }
  const latency = 40 + ((n.host.length * 37 + n.port) % 400)
  return {
    ...n, status: 'healthy', latencyMs: latency, lastCheckedAt: now, lastError: null,
    exitIp: `203.0.113.${(n.port % 200) + 1}`, successCount: n.successCount + 1,
  }
}

export const proxyMock: ProxyApi = {
  getProxyPool: async () => state(),
  saveProxyPoolConfig: async (c) => {
    config = { ...c, providerIds: [...c.providerIds] }
    return state()
  },
  saveProxyNode: async (input) => {
    if (input.id) {
      const i = nodes.findIndex((n) => n.id === input.id)
      if (i < 0) throw new Error('代理节点不存在')
      const prev = nodes[i]
      const next = blankNode(input, prev.id)
      next.hasPassword = !!next.username && (input.password === undefined ? prev.hasPassword : input.password !== '')
      nodes[i] = next
    } else {
      nodes.push(blankNode(input, `px-${++seq}`))
    }
    return state()
  },
  deleteProxyNode: async (id) => {
    nodes = nodes.filter((n) => n.id !== id)
    return state()
  },
  setProxyNodeEnabled: async (id, enabled) => {
    nodes = nodes.map((n) => (n.id === id ? { ...n, enabled } : n))
    return state()
  },
  moveProxyNode: async (id, delta) => {
    const i = nodes.findIndex((n) => n.id === id)
    const j = i + Math.sign(delta)
    if (i >= 0 && j >= 0 && j < nodes.length) [nodes[i], nodes[j]] = [nodes[j], nodes[i]]
    return state()
  },
  importProxyNodes: async (text) => {
    const result = { added: 0, skipped: [] as { line: number; reason: string }[] }
    text.split('\n').forEach((raw, idx) => {
      const line = raw.trim()
      if (!line || line.startsWith('#')) return
      const parsed = parseLine(line)
      if (typeof parsed === 'string') {
        result.skipped.push({ line: idx + 1, reason: parsed })
        return
      }
      const dup = nodes.some((n) => n.scheme === parsed.scheme && n.host === parsed.host && n.port === parsed.port && n.username === (parsed.username ?? null))
      if (dup) {
        result.skipped.push({ line: idx + 1, reason: '已存在相同的代理' })
        return
      }
      nodes.push(blankNode(parsed, `px-${++seq}`))
      result.added += 1
    })
    return result
  },
  testProxyNodes: async (ids) => {
    nodes = nodes.map((n) => (!ids || ids.includes(n.id) ? fakeTest(n) : n))
    return nodes.map((n) => ({ ...n }))
  },
}
