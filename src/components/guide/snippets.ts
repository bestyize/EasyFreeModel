/**
 * 接入指南的全部文本生成。纯函数，不碰 DOM 和网络，便于逐条对照各客户端的官方格式做单元测试：
 * - Codex：custom provider 在 config.toml 的 [model_providers.<id>]，wire_api 目前只支持 "responses"。
 *   这是本机客户端，图省事：密钥直接写 experimental_bearer_token（0.100 到 0.160 都支持），不用再设环境变量。
 * - Claude Code：~/.claude/settings.json 的 env；ANTHROPIC_BASE_URL 不带 /v1，ANTHROPIC_AUTH_TOKEN 以 Bearer 发送。
 * - CC Switch：ccswitch://v1/import?resource=provider&app=…，参数 URL 编码；Codex 的 endpoint 带 /v1，Claude 的不带。
 */
import type { Protocol } from '@/types'

export const KEY_PLACEHOLDER = 'YOUR_API_KEY'
export const MODEL_PLACEHOLDER = 'YOUR_MODEL'
export const PROVIDER_NAME = 'FreeApiBox'
const CODEX_PROVIDER_ID = 'freeapibox'
const PROMPT = 'Hello'

export interface GuideInput {
  /** 网关根地址，形如 http://127.0.0.1:20128，不带结尾斜杠 */
  origin: string
  key: string
  model: string
  haikuModel?: string
  sonnetModel?: string
  opusModel?: string
}

/** 把监听地址变成客户端可连的根地址。通配地址（0.0.0.0 / ::）对本机客户端等价于回环。 */
export function gatewayOrigin(address: string | null | undefined, port: number): string {
  let host = (address ?? '').trim().replace(/^\[|\]$/g, '')
  if (!host || host === '0.0.0.0' || host === '::') host = '127.0.0.1'
  if (host.includes(':')) host = `[${host}]`
  return `http://${host}:${port}`
}

/** 监听在通配或非回环地址时，局域网内其它设备需要把主机换成本机的局域网 IP。 */
export function isWildcardAddress(address: string | null | undefined): boolean {
  const a = (address ?? '').trim().replace(/^\[|\]$/g, '')
  return a === '0.0.0.0' || a === '::'
}

// ───────── 转义 ─────────

/** POSIX shell 单引号字符串 */
function sh(s: string): string {
  return `'${s.replace(/'/g, `'\\''`)}'`
}

/** JSON 字符串字面量，同时是合法的 Python / JavaScript / TOML 基础字符串 */
const q = (s: string) => JSON.stringify(s)

const pretty = (v: unknown) => JSON.stringify(v, null, 2)

// ───────── 地址表 ─────────

export interface EndpointRow {
  id: string
  label: string
  url: string
  /** 填进 SDK / 客户端“Base URL”栏的值；没有对应概念的行为空 */
  baseUrl?: string
}

export function endpointRows(origin: string): EndpointRow[] {
  return [
    { id: 'openai-chat', label: 'OpenAI Chat Completions', url: `${origin}/v1/chat/completions`, baseUrl: `${origin}/v1` },
    { id: 'openai-responses', label: 'OpenAI Responses', url: `${origin}/v1/responses`, baseUrl: `${origin}/v1` },
    { id: 'anthropic-messages', label: 'Anthropic Messages', url: `${origin}/v1/messages`, baseUrl: origin },
    { id: 'gemini', label: 'Gemini', url: `${origin}/v1beta/models/{model}:generateContent`, baseUrl: origin },
    { id: 'models', label: '模型列表', url: `${origin}/v1/models` },
  ]
}

// ───────── 请求示例 ─────────

export type ExampleProtocol = Extract<Protocol, 'openai-chat' | 'openai-responses' | 'anthropic-messages' | 'gemini'>
export type ExampleLang = 'curl' | 'python' | 'node'

export const EXAMPLE_PROTOCOLS: { id: ExampleProtocol; label: string }[] = [
  { id: 'openai-chat', label: 'OpenAI Chat' },
  { id: 'openai-responses', label: 'OpenAI Responses' },
  { id: 'anthropic-messages', label: 'Anthropic' },
  { id: 'gemini', label: 'Gemini' },
]

export const EXAMPLE_LANGS: { id: ExampleLang; label: string }[] = [
  { id: 'curl', label: 'curl' },
  { id: 'python', label: 'Python' },
  { id: 'node', label: 'Node.js' },
]

/** Gemini 只提供 curl：官方 SDK 的自定义地址写法随版本变化，不给出可能过期的代码。 */
export function langsFor(protocol: ExampleProtocol): ExampleLang[] {
  return protocol === 'gemini' ? ['curl'] : ['curl', 'python', 'node']
}

function curlExample(p: ExampleProtocol, { origin, key, model }: GuideInput): string {
  const lines = (...parts: string[]) => parts.join(' \\\n  ')
  switch (p) {
    case 'openai-chat':
      return lines(
        `curl ${origin}/v1/chat/completions`,
        `-H ${sh(`Authorization: Bearer ${key}`)}`,
        `-H 'Content-Type: application/json'`,
        `-d ${sh(pretty({ model, messages: [{ role: 'user', content: PROMPT }] }))}`,
      )
    case 'openai-responses':
      return lines(
        `curl ${origin}/v1/responses`,
        `-H ${sh(`Authorization: Bearer ${key}`)}`,
        `-H 'Content-Type: application/json'`,
        `-d ${sh(pretty({ model, input: PROMPT }))}`,
      )
    case 'anthropic-messages':
      return lines(
        `curl ${origin}/v1/messages`,
        `-H ${sh(`x-api-key: ${key}`)}`,
        `-H 'anthropic-version: 2023-06-01'`,
        `-H 'Content-Type: application/json'`,
        `-d ${sh(pretty({ model, max_tokens: 1024, messages: [{ role: 'user', content: PROMPT }] }))}`,
      )
    case 'gemini':
      return lines(
        `curl ${sh(`${origin}/v1beta/models/${model}:generateContent`)}`,
        `-H ${sh(`x-goog-api-key: ${key}`)}`,
        `-H 'Content-Type: application/json'`,
        `-d ${sh(pretty({ contents: [{ parts: [{ text: PROMPT }] }] }))}`,
      )
  }
}

function pythonExample(p: Exclude<ExampleProtocol, 'gemini'>, { origin, key, model }: GuideInput): string {
  switch (p) {
    case 'openai-chat':
      return `from openai import OpenAI

client = OpenAI(base_url=${q(`${origin}/v1`)}, api_key=${q(key)})
resp = client.chat.completions.create(
    model=${q(model)},
    messages=[{"role": "user", "content": ${q(PROMPT)}}],
)
print(resp.choices[0].message.content)`
    case 'openai-responses':
      return `from openai import OpenAI

client = OpenAI(base_url=${q(`${origin}/v1`)}, api_key=${q(key)})
resp = client.responses.create(model=${q(model)}, input=${q(PROMPT)})
print(resp.output_text)`
    case 'anthropic-messages':
      return `import anthropic

client = anthropic.Anthropic(base_url=${q(origin)}, api_key=${q(key)})
msg = client.messages.create(
    model=${q(model)},
    max_tokens=1024,
    messages=[{"role": "user", "content": ${q(PROMPT)}}],
)
print(msg.content[0].text)`
  }
}

function nodeExample(p: Exclude<ExampleProtocol, 'gemini'>, { origin, key, model }: GuideInput): string {
  switch (p) {
    case 'openai-chat':
      return `import OpenAI from "openai";

const client = new OpenAI({ baseURL: ${q(`${origin}/v1`)}, apiKey: ${q(key)} });
const resp = await client.chat.completions.create({
  model: ${q(model)},
  messages: [{ role: "user", content: ${q(PROMPT)} }],
});
console.log(resp.choices[0].message.content);`
    case 'openai-responses':
      return `import OpenAI from "openai";

const client = new OpenAI({ baseURL: ${q(`${origin}/v1`)}, apiKey: ${q(key)} });
const resp = await client.responses.create({ model: ${q(model)}, input: ${q(PROMPT)} });
console.log(resp.output_text);`
    case 'anthropic-messages':
      return `import Anthropic from "@anthropic-ai/sdk";

const client = new Anthropic({ baseURL: ${q(origin)}, apiKey: ${q(key)} });
const msg = await client.messages.create({
  model: ${q(model)},
  max_tokens: 1024,
  messages: [{ role: "user", content: ${q(PROMPT)} }],
});
console.log(msg.content[0].text);`
  }
}

/** 该协议 + 语言组合没有示例时返回 null */
export function requestExample(protocol: ExampleProtocol, lang: ExampleLang, input: GuideInput): string | null {
  if (!langsFor(protocol).includes(lang)) return null
  if (lang === 'curl') return curlExample(protocol, input)
  const p = protocol as Exclude<ExampleProtocol, 'gemini'>
  return lang === 'python' ? pythonExample(p, input) : nodeExample(p, input)
}

// ───────── Codex ─────────

export function codexConfigToml({ origin, key, model }: GuideInput): string {
  return `model = ${q(model)}
model_provider = ${q(CODEX_PROVIDER_ID)}

[model_providers.${CODEX_PROVIDER_ID}]
name = ${q(PROVIDER_NAME)}
base_url = ${q(`${origin}/v1`)}
wire_api = "responses"
experimental_bearer_token = ${q(key)}`
}

// ───────── Claude Code ─────────

export function claudeSettingsJson({ origin, key, model, haikuModel, sonnetModel, opusModel }: GuideInput): string {
  return pretty({
    env: {
      ANTHROPIC_BASE_URL: origin,
      ANTHROPIC_AUTH_TOKEN: key,
      ANTHROPIC_MODEL: model,
      ANTHROPIC_DEFAULT_HAIKU_MODEL: haikuModel || model,
      ANTHROPIC_DEFAULT_SONNET_MODEL: sonnetModel || model,
      ANTHROPIC_DEFAULT_OPUS_MODEL: opusModel || model,
    },
  })
}

// ───────── CC Switch ─────────

export type CcSwitchApp = 'claude' | 'codex'

/** CC Switch 里 Claude 填根地址，Codex 填带 /v1 的 Base URL */
export function ccSwitchEndpoint(app: CcSwitchApp, origin: string): string {
  return app === 'codex' ? `${origin}/v1` : origin
}

/**
 * `ccswitch://v1/import?resource=provider&app=…`。
 * 参数用 encodeURIComponent（空格为 %20，与官方示例一致）。Claude 额外填三个档位模型，避免后台任务请求不存在的 Haiku 模型。
 */
export function ccSwitchLink(app: CcSwitchApp, { origin, key, model, haikuModel, sonnetModel, opusModel }: GuideInput): string {
  const params: [string, string][] = [
    ['resource', 'provider'],
    ['app', app],
    ['name', PROVIDER_NAME],
    ['endpoint', ccSwitchEndpoint(app, origin)],
    ['apiKey', key],
    ['model', model],
  ]
  if (app === 'claude') {
    params.push(['haikuModel', haikuModel || model], ['sonnetModel', sonnetModel || model], ['opusModel', opusModel || model])
  }
  return `ccswitch://v1/import?${params.map(([k, v]) => `${k}=${encodeURIComponent(v)}`).join('&')}`
}
