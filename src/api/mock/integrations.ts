import type { IntegrationsApi } from '../types'
import type { ClientImportInput, ImportFieldChange } from '@/types'

/** 浏览器预览用 mock：不碰任何真实文件，只模拟“先预览、确认后写入、再次预览为已是最新”的流程。 */
const applied = new Map<string, string>()

const fingerprint = (i: ClientImportInput) => JSON.stringify([i.origin, i.apiKey, i.model, i.haikuModel, i.sonnetModel, i.opusModel])
const mask = (s: string) => (s.length <= 8 ? '••••' : `${s.slice(0, 4)}…${s.slice(-4)}`)

function changes(i: ClientImportInput, done: boolean): ImportFieldChange[] {
  const old = (v: string) => (done ? v : null)
  if (i.client === 'codex') {
    return [
      { field: 'model', before: done ? i.model : 'gpt-5', after: i.model },
      { field: 'model_provider', before: done ? 'freeapibox' : 'custom', after: 'freeapibox' },
      { field: 'model_providers.freeapibox.name', before: old('FreeApiBox'), after: 'FreeApiBox' },
      { field: 'model_providers.freeapibox.base_url', before: old(`${i.origin}/v1`), after: `${i.origin}/v1` },
      { field: 'model_providers.freeapibox.wire_api', before: old('responses'), after: 'responses' },
      { field: 'model_providers.freeapibox.experimental_bearer_token', before: old(mask(i.apiKey)), after: i.apiKey },
    ]
  }
  return [
    ['ANTHROPIC_BASE_URL', i.origin],
    ['ANTHROPIC_AUTH_TOKEN', i.apiKey],
    ['ANTHROPIC_MODEL', i.model],
    ['ANTHROPIC_DEFAULT_HAIKU_MODEL', i.haikuModel || i.model],
    ['ANTHROPIC_DEFAULT_SONNET_MODEL', i.sonnetModel || i.model],
    ['ANTHROPIC_DEFAULT_OPUS_MODEL', i.opusModel || i.model],
  ].map(([k, v]) => ({
    field: `env.${k}`,
    before: done ? (k === 'ANTHROPIC_AUTH_TOKEN' ? mask(v) : v) : k === 'ANTHROPIC_BASE_URL' ? 'https://old.example.com' : null,
    after: v,
  }))
}

const pathOf = (c: ClientImportInput['client']) => (c === 'codex' ? '~/.codex/config.toml' : '~/.claude/settings.json')

export const integrationsMock: IntegrationsApi = {
  previewClientImport: async (input) => {
    const done = applied.get(input.client) === fingerprint(input)
    return { client: input.client, path: pathOf(input.client), exists: true, unchanged: done, changes: changes(input, done), error: null }
  },
  applyClientImport: async (input) => {
    const done = applied.get(input.client) === fingerprint(input)
    applied.set(input.client, fingerprint(input))
    return {
      path: pathOf(input.client),
      backupPath: done ? null : `${pathOf(input.client)}.router-bak-20261015-103000`,
      changed: !done,
    }
  },
}
