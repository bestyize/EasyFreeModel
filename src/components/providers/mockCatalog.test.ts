import { beforeEach, describe, expect, it } from 'vitest'
import catalogJson from '../../../src-tauri/resources/provider_catalog.json'
import statusJson from '../../../src-tauri/resources/provider_status.json'
import modelsJson from '../../../src-tauri/resources/provider_models.json'
import { __resetProvidersMock, __setProvidersMockLatency, providersMock as mock } from '@/api/mock/providers'

const catalog = catalogJson as unknown as { id: string; category: string; serviceKinds: string[]; color: string | null }[]
const status = statusJson as unknown as Record<string, { adapter: string | null; reason: string | null; baseUrl: string | null }>
const curated = modelsJson as unknown as Record<string, { id: string; upstreamModelId: string; publicAlias: string }[]>

const SECRET = 'sk-mock-SUPERSECRET-0001'

beforeEach(() => {
  __setProvidersMockLatency(0)
  __resetProvidersMock()
})

describe('mock 目录与真实资源一致', () => {
  it('134 家，分组 23 / 24 / 87，顺序自定义 → OAuth → 免费 → API Key', async () => {
    const list = await mock.listProviders()
    expect(list).toHaveLength(134)
    expect(list.filter((p) => p.group === 'oauth')).toHaveLength(23)
    expect(list.filter((p) => p.group === 'free')).toHaveLength(24)
    expect(list.filter((p) => p.group === 'apikey')).toHaveLength(87)
    const rank = { custom: 0, oauth: 1, free: 2, apikey: 3 } as const
    expect(list.every((p, i) => i === 0 || rank[list[i - 1].group] <= rank[p.group])).toBe(true)
    expect(new Set(list.map((p) => p.id)).size).toBe(134)
    expect(catalog.map((c) => c.id).sort()).toEqual(list.map((p) => p.id).sort())
  })

  it('adapterReady 与 provider_status.json 一致，待接入必有原因', async () => {
    const list = await mock.listProviders()
    for (const p of list) {
      const st = status[p.id]
      expect(p.adapterReady, p.id).toBe(st.adapter !== null)
      if (p.adapterReady) {
        expect(p.pendingReason, p.id).toBeNull()
      } else {
        expect(p.pendingReason, p.id).toBe(st.reason)
        expect((p.pendingReason ?? '').trim(), p.id).not.toBe('')
        expect(p.status).toBe('unsupported')
      }
    }
    expect(list.filter((p) => p.adapterReady).length).toBe(Object.values(status).filter((s) => s.adapter).length)
  })

  it('目录元数据：颜色规范化、未声明服务类型按 llm、authModes 使用 apiKey 写法', async () => {
    const byId = Object.fromEntries((await mock.listProviders()).map((p) => [p.id, p]))
    expect(byId['chutes'].color).toBe('#FFFFFF')
    expect(byId['alicode-intl'].color).toBe('#FF6A00')
    expect(byId['alicode-intl'].textIcon).toBe('ALi')
    expect(byId['azure'].serviceKinds).toEqual(['llm'])
    expect(byId['openai'].authModes).toEqual(['apiKey'])
    expect(byId['clinepass'].authModes).toEqual(['apiKey', 'oauth'])
    expect(byId['opencode'].authModes).toEqual(['none'])
    expect(byId['opencode-zen'].authModes).toEqual(['apiKey'])
    expect(byId['edge-tts'].website).toBeNull()
    for (const p of Object.values(byId)) expect(p.color === null || /^#[0-9A-F]{6}$/.test(p.color), p.id).toBe(true)
  })

  it('风险提示只给 OAuth/订阅类，且不是占位字符串', async () => {
    expect((await mock.getProvider('openrouter')).riskNotice).toBeNull()
    expect((await mock.getProvider('elevenlabs')).riskNotice).toBeNull()
    for (const id of ['claude', 'gemini-cli']) {
      const n = (await mock.getProvider(id)).riskNotice
      expect(n).toContain('OAuth')
      expect(n).not.toBe('RISK_NOTICE')
    }
  })
})

describe('mock 命令遵守同样的红线', () => {
  it('免密钥提供商一键启用，重复操作不建第二个连接，也不接受密钥', async () => {
    await expect(mock.addConnection({ providerId: 'opencode', accountLabel: '', apiKey: SECRET })).rejects.toThrow('无需 API Key')
    const input = { providerId: 'opencode', accountLabel: '免密钥连接', apiKey: '' }
    const conn = await mock.addConnection(input)
    expect(conn.kind).toBe('none')
    expect((await mock.addConnection(input)).id).toBe(conn.id)
    expect((await mock.getProvider('opencode')).connections).toHaveLength(1)
    expect((await mock.testConnection(conn.id)).error).toContain('无法确认可用')
    expect((await mock.listProviders()).find((p) => p.id === 'opencode')?.status).toBe('connected')
  })
  it('addConnection：未就绪的提供商被拒绝并说明原因', async () => {
    await expect(mock.addConnection({ providerId: 'grok-web', accountLabel: 'x', apiKey: SECRET })).rejects.toThrow(
      '网页会话类暂缓',
    )
    await expect(mock.addConnection({ providerId: 'elevenlabs', accountLabel: 'x', apiKey: SECRET })).rejects.toThrow(
      '媒体适配器由媒体模块提供',
    )
  })

  it('addConnection：校验输入，成功返回的 Connection 与后续任何读取都不含密钥', async () => {
    await expect(mock.addConnection({ providerId: 'openai', accountLabel: 'x', apiKey: '   ' })).rejects.toThrow('API Key')
    await expect(mock.addConnection({ providerId: 'openai', accountLabel: ' ', apiKey: SECRET })).rejects.toThrow('名称')
    const conn = await mock.addConnection({ providerId: 'openai', accountLabel: '  work  ', apiKey: ` ${SECRET} ` })
    expect(conn.accountLabel).toBe('work')
    expect(conn.kind).toBe('apiKey')
    const dump = JSON.stringify([conn, await mock.getProvider('openai'), await mock.listProviders()])
    expect(dump).not.toContain('SUPERSECRET')
    expect((await mock.listProviders()).find((p) => p.id === 'openai')?.connectionCount).toBe(1)
  })

  it('testProviders / testConnection 永远不伪造成功', async () => {
    const conn = await mock.addConnection({ providerId: 'openai', accountLabel: 'k', apiKey: SECRET })
    const res = await mock.testProviders(['grok-web', 'openai', 'mistral', 'openrouter', 'nope', 'grok-web'])
    expect(res.every((r) => !r.ok)).toBe(true)
    const of = (id: string) => res.filter((r) => r.providerId === id)
    expect(of('grok-web')).toHaveLength(1)
    expect(of('grok-web')[0].error).toContain('网页会话类暂缓')
    expect(of('openai')[0].connectionId).toBe(conn.id)
    expect(of('openai')[0].error).toContain('无法确认可用')
    expect(of('mistral')[0].error).toContain('尚未添加连接')
    expect(of('nope')[0].error).toContain('不存在')
    expect((await mock.testConnection(conn.id)).ok).toBe(false)
    expect(JSON.stringify(res)).not.toContain('SUPERSECRET')
  })

  it('自定义提供商：校验协议与 URL，可创建、可保存、可删除；内置不可删', async () => {
    await expect(mock.createCustomProvider({ name: '', protocol: 'openai-chat', baseUrl: 'https://a.example.com' })).rejects.toThrow('名称')
    await expect(mock.createCustomProvider({ name: 'X', protocol: 'openai-chat', baseUrl: 'http://a.example.com' })).rejects.toThrow('https')
    await expect(
      mock.createCustomProvider({ name: 'X', protocol: 'unsupported' as never, baseUrl: 'https://a.example.com' }),
    ).rejects.toThrow('协议')

    const d = await mock.createCustomProvider({ name: ' Mine ', protocol: 'anthropic-messages', baseUrl: 'https://a.example.com/v1/' })
    expect(d.group).toBe('custom')
    expect(d.name).toBe('Mine')
    expect(d.adapterReady).toBe(true)
    expect(d.baseUrl).toBe('https://a.example.com/v1')
    expect((await mock.listProviders())[0].id).toBe(d.id)

    const responses = await mock.createCustomProvider({ name: 'Responses', protocol: 'openai-responses', baseUrl: 'https://a.example.com/v1' })
    expect(responses.protocols).toEqual(['openai-responses'])

    await expect(mock.deleteCustomProvider('openrouter')).rejects.toThrow('自定义')
    await mock.deleteCustomProvider(d.id)
    await expect(mock.getProvider(d.id)).rejects.toThrow()
  })

  it('saveProvider 不接受待接入提供商的写入，也不允许客户端改写 adapterReady', async () => {
    const unready = await mock.getProvider('grok-web')
    await expect(mock.saveProvider({ ...unready, adapterReady: true })).rejects.toThrow('待接入')
    const or = await mock.getProvider('openrouter')
    const saved = await mock.saveProvider({ ...or, adapterReady: false, name: 'Hacked', customHeaders: [{ key: 'X-T', value: 'secret-value', secret: true }] })
    expect(saved.adapterReady).toBe(true)
    expect(saved.name).toBe('OpenRouter')
    expect(JSON.stringify(saved)).not.toContain('secret-value')
  })
})

describe('预设模型与拉取模型', () => {
  it('绝大多数提供商预置了可用模型（与内置模型库一致）', async () => {
    const withModels = Object.entries(curated).filter(([, ms]) => ms.length)
    expect(withModels.length).toBeGreaterThan(60)
    // 抽样核对：预设模型与 mock 详情一致
    for (const id of ['antigravity', 'openai', 'claude', 'gemini']) {
      const d = await mock.getProvider(id)
      expect(d.models.length, id).toBe(curated[id].length)
      expect(d.models.map((m) => m.publicAlias)).toEqual(curated[id].map((m) => m.publicAlias))
    }
  })

  it('fetchProviderModels 合并内置模型库，且保留用户已改的启用状态与别名', async () => {
    const before = await mock.getProvider('antigravity')
    expect(before.models.length).toBeGreaterThan(0)

    // 用户把第一个模型改名并禁用
    const first = before.models[0]
    await mock.saveProvider({
      ...before,
      models: [{ ...first, publicAlias: 'my-alias', enabled: false }, ...before.models.slice(1)],
    })

    const fetched = await mock.fetchProviderModels('antigravity')
    const kept = fetched.find((m) => m.id === first.id)!
    expect(kept.publicAlias).toBe('my-alias')
    expect(kept.enabled).toBe(false)
    expect(fetched.length).toBe(curated['antigravity'].length)
  })

  it('没有预设模型的提供商：拉取后仍为空数组，不伪造模型', async () => {
    // 找一个既没有预设模型、也没有连接的提供商
    const empty = catalog.find((c) => !curated[c.id] && status[c.id].adapter)?.id
    if (empty) {
      const list = await mock.fetchProviderModels(empty)
      expect(list).toEqual([])
    }
  })
})
