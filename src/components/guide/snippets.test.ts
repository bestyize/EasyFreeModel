import { describe, expect, it } from 'vitest'
import {
  EXAMPLE_PROTOCOLS,
  ccSwitchLink,
  ccSwitchEndpoint,
  claudeSettingsJson,
  codexConfigToml,
  endpointRows,
  gatewayOrigin,
  isWildcardAddress,
  langsFor,
  requestExample,
  type GuideInput,
} from './snippets'

const input: GuideInput = { origin: 'http://127.0.0.1:20128', key: 'sk-r-abc123', model: 'openai/gpt-4o-mini' }

describe('gatewayOrigin', () => {
  it('通配地址对本机客户端换成回环', () => {
    expect(gatewayOrigin('0.0.0.0', 20128)).toBe('http://127.0.0.1:20128')
    expect(gatewayOrigin('::', 20128)).toBe('http://127.0.0.1:20128')
    expect(gatewayOrigin('', 1)).toBe('http://127.0.0.1:1')
    expect(gatewayOrigin(undefined, 1)).toBe('http://127.0.0.1:1')
  })
  it('保留具体地址，IPv6 加方括号', () => {
    expect(gatewayOrigin('localhost', 8080)).toBe('http://localhost:8080')
    expect(gatewayOrigin('192.168.1.5', 8080)).toBe('http://192.168.1.5:8080')
    expect(gatewayOrigin('::1', 8080)).toBe('http://[::1]:8080')
    expect(gatewayOrigin('[::1]', 8080)).toBe('http://[::1]:8080')
  })
  it('isWildcardAddress', () => {
    expect(isWildcardAddress('0.0.0.0')).toBe(true)
    expect(isWildcardAddress('[::]')).toBe(true)
    expect(isWildcardAddress('127.0.0.1')).toBe(false)
  })
})

describe('endpointRows', () => {
  it('列出各协议地址，OpenAI 系 Base URL 带 /v1，Anthropic / Gemini 不带', () => {
    const rows = Object.fromEntries(endpointRows(input.origin).map((r) => [r.id, r]))
    expect(rows['openai-chat'].url).toBe('http://127.0.0.1:20128/v1/chat/completions')
    expect(rows['openai-chat'].baseUrl).toBe('http://127.0.0.1:20128/v1')
    expect(rows['openai-responses'].url).toBe('http://127.0.0.1:20128/v1/responses')
    expect(rows['anthropic-messages'].url).toBe('http://127.0.0.1:20128/v1/messages')
    expect(rows['anthropic-messages'].baseUrl).toBe('http://127.0.0.1:20128')
    expect(rows.models.baseUrl).toBeUndefined()
    expect(rows.gemini.url).toContain('/v1beta/models/{model}:generateContent')
    expect(rows.models.url).toBe('http://127.0.0.1:20128/v1/models')
  })
})

describe('requestExample', () => {
  it('curl：OpenAI Chat 用 Bearer，body 是合法 JSON', () => {
    const t = requestExample('openai-chat', 'curl', input)!
    expect(t).toContain('curl http://127.0.0.1:20128/v1/chat/completions')
    expect(t).toContain("-H 'Authorization: Bearer sk-r-abc123'")
    const body = t.slice(t.indexOf("-d '") + 4, t.lastIndexOf("'"))
    expect(JSON.parse(body)).toEqual({ model: input.model, messages: [{ role: 'user', content: 'Hello' }] })
  })

  it('curl：Responses / Anthropic / Gemini 各用自己的路径与认证头', () => {
    expect(requestExample('openai-responses', 'curl', input)).toContain('/v1/responses')
    const a = requestExample('anthropic-messages', 'curl', input)!
    expect(a).toContain('/v1/messages')
    expect(a).toContain("-H 'x-api-key: sk-r-abc123'")
    expect(a).toContain("-H 'anthropic-version: 2023-06-01'")
    expect(a).toContain('"max_tokens": 1024')
    const g = requestExample('gemini', 'curl', input)!
    expect(g).toContain("curl 'http://127.0.0.1:20128/v1beta/models/openai/gpt-4o-mini:generateContent'")
    expect(g).toContain("-H 'x-goog-api-key: sk-r-abc123'")
  })

  it('Python / Node：OpenAI SDK 的 base_url 带 /v1，Anthropic SDK 的不带', () => {
    expect(requestExample('openai-chat', 'python', input)).toContain('base_url="http://127.0.0.1:20128/v1"')
    expect(requestExample('openai-responses', 'python', input)).toContain('client.responses.create(')
    expect(requestExample('anthropic-messages', 'python', input)).toContain('base_url="http://127.0.0.1:20128", api_key="sk-r-abc123"')
    expect(requestExample('openai-chat', 'node', input)).toContain('baseURL: "http://127.0.0.1:20128/v1"')
    expect(requestExample('openai-responses', 'node', input)).toContain('resp.output_text')
    expect(requestExample('anthropic-messages', 'node', input)).toContain('baseURL: "http://127.0.0.1:20128"')
  })

  it('Gemini 只有 curl，其它组合返回 null', () => {
    expect(langsFor('gemini')).toEqual(['curl'])
    expect(requestExample('gemini', 'python', input)).toBeNull()
    expect(requestExample('gemini', 'node', input)).toBeNull()
    for (const p of EXAMPLE_PROTOCOLS) expect(requestExample(p.id, 'curl', input)).toBeTruthy()
  })

  it('密钥里的单引号在 shell 中被正确转义，引号和反斜杠在代码字符串中被转义', () => {
    const evil: GuideInput = { ...input, key: `a'b"c\\d` }
    expect(requestExample('openai-chat', 'curl', evil)).toContain(`-H 'Authorization: Bearer a'\\''b"c\\d'`)
    expect(requestExample('openai-chat', 'python', evil)).toContain(`api_key="a'b\\"c\\\\d"`)
    expect(requestExample('openai-chat', 'node', evil)).toContain(`apiKey: "a'b\\"c\\\\d"`)
  })
})

describe('Codex', () => {
  it('config.toml：custom provider 走 responses，密钥直接写 experimental_bearer_token（不用环境变量）', () => {
    const t = codexConfigToml(input)
    expect(t).toBe(`model = "openai/gpt-4o-mini"
model_provider = "router"

[model_providers.router]
name = "Router Desktop"
base_url = "http://127.0.0.1:20128/v1"
wire_api = "responses"
experimental_bearer_token = "sk-r-abc123"`)
    expect(t).not.toContain('env_key')
    expect(t.indexOf('model_provider =')).toBeLessThan(t.indexOf('[model_providers.router]'))
  })

  it('密钥里的引号和反斜杠被转义成合法的 TOML 字符串', () => {
    expect(codexConfigToml({ ...input, key: 'a"b\\c' })).toContain('experimental_bearer_token = "a\\"b\\\\c"')
  })
})

describe('Claude Code', () => {
  it('settings.json：env 里 Base URL 不带 /v1，用 AUTH_TOKEN（Bearer），并把三个档位模型都指向所选模型', () => {
    const parsed = JSON.parse(claudeSettingsJson(input))
    expect(parsed).toEqual({
      env: {
        ANTHROPIC_BASE_URL: 'http://127.0.0.1:20128',
        ANTHROPIC_AUTH_TOKEN: 'sk-r-abc123',
        ANTHROPIC_MODEL: 'openai/gpt-4o-mini',
        ANTHROPIC_DEFAULT_HAIKU_MODEL: 'openai/gpt-4o-mini',
        ANTHROPIC_DEFAULT_SONNET_MODEL: 'openai/gpt-4o-mini',
        ANTHROPIC_DEFAULT_OPUS_MODEL: 'openai/gpt-4o-mini',
      },
    })
  })

})

describe('ccSwitchEndpoint', () => {
  it('Claude 用根地址，Codex 带 /v1', () => {
    expect(ccSwitchEndpoint('claude', input.origin)).toBe(input.origin)
    expect(ccSwitchEndpoint('codex', input.origin)).toBe(`${input.origin}/v1`)
  })
})

describe('ccSwitchLink', () => {
  const parse = (link: string) => {
    const u = new URL(link)
    return { host: u.host, path: u.pathname, params: Object.fromEntries(u.searchParams) }
  }

  it('Claude：endpoint 不带 /v1，带三个档位模型', () => {
    const link = ccSwitchLink('claude', input)
    expect(link.startsWith('ccswitch://v1/import?resource=provider&app=claude&name=Router%20Desktop&')).toBe(true)
    expect(parse(link)).toEqual({
      host: 'v1',
      path: '/import',
      params: {
        resource: 'provider', app: 'claude', name: 'Router Desktop', endpoint: 'http://127.0.0.1:20128', apiKey: 'sk-r-abc123',
        model: 'openai/gpt-4o-mini', haikuModel: 'openai/gpt-4o-mini', sonnetModel: 'openai/gpt-4o-mini', opusModel: 'openai/gpt-4o-mini',
      },
    })
  })

  it('Codex：endpoint 带 /v1，没有 Claude 专属参数', () => {
    const { params } = parse(ccSwitchLink('codex', input))
    expect(params).toEqual({
      resource: 'provider', app: 'codex', name: 'Router Desktop', endpoint: 'http://127.0.0.1:20128/v1', apiKey: 'sk-r-abc123',
      model: 'openai/gpt-4o-mini',
    })
  })

  it('特殊字符被完整编码并可无损还原', () => {
    const odd: GuideInput = { origin: 'http://[::1]:1', key: 'k&y=1+2 #%', model: 'm/é 中' }
    const { params } = parse(ccSwitchLink('claude', odd))
    expect(params.apiKey).toBe('k&y=1+2 #%')
    expect(params.model).toBe('m/é 中')
    expect(params.endpoint).toBe('http://[::1]:1')
    expect(ccSwitchLink('claude', odd)).not.toMatch(/ |#/)
  })
})
