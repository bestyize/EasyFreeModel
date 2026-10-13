import { describe, expect, it } from 'vitest'
import { cloneDetail, findAliasConflicts, isDetailDirty, validateDetail } from './draft'
import type { ModelEntry, ProviderDetail } from '@/types'

function m(id: string, alias: string, enabled = true): ModelEntry {
  return { id, upstreamModelId: id, publicAlias: alias, description: '', enabled, supportsThinking: true }
}

function detail(models: ModelEntry[] = []): ProviderDetail {
  return {
    id: 'p1',
    name: 'P1',
    group: 'oauth',
    status: 'connected',
    connectionCount: 0,
    adapterReady: true,
    protocols: ['openai-chat'],
    quotaHint: null,
    serviceKinds: ['llm'],
    website: null,
    color: null,
    textIcon: null,
    authModes: ['apiKey'],
    pendingReason: null,
    riskNotice: null,
    connections: [],
    distribution: { roundRobin: true, maxConcurrency: 4, maxRetries: 2 },
    customHeaders: [],
    models,
    thinkingLevel: 'auto',
  }
}

describe('findAliasConflicts', () => {
  it('detects duplicate aliases among enabled models (case/space insensitive)', () => {
    const c = findAliasConflicts([m('a', 'x/foo'), m('b', ' X/Foo '), m('c', 'x/bar')])
    expect([...c.values()]).toEqual([['a', 'b']])
  })
  it('ignores disabled models', () => {
    expect(findAliasConflicts([m('a', 'x/foo'), m('b', 'x/foo', false)]).size).toBe(0)
  })
})

describe('validateDetail', () => {
  it('reports alias conflict', () => {
    const issues = validateDetail(detail([m('a', 'dup'), m('b', 'dup')]))
    expect(issues.some((i) => i.field === 'alias' && i.message.includes('冲突'))).toBe(true)
  })
  it('passes for clean detail', () => {
    expect(validateDetail(detail([m('a', 'one'), m('b', 'two')]))).toEqual([])
  })
  it('rejects duplicate header keys and bad distribution', () => {
    const d = detail()
    d.customHeaders = [
      { key: 'X-A', value: '1', secret: false },
      { key: 'x-a', value: '2', secret: true },
    ]
    d.distribution.maxConcurrency = 0
    const fields = validateDetail(d).map((i) => i.field)
    expect(fields).toContain('headers')
    expect(fields).toContain('distribution')
  })
  it('requires a name for custom providers', () => {
    const d = detail()
    d.group = 'custom'
    d.name = '  '
    expect(validateDetail(d).some((i) => i.field === 'name')).toBe(true)
  })
})

describe('isDetailDirty', () => {
  it('is clean for an identical clone', () => {
    const d = detail([m('a', 'one')])
    expect(isDetailDirty(d, cloneDetail(d))).toBe(false)
  })
  it('becomes dirty after toggling a model, and clean again when reverted', () => {
    const d = detail([m('a', 'one')])
    const c = cloneDetail(d)
    c.models[0].enabled = false
    expect(isDetailDirty(d, c)).toBe(true)
    c.models[0].enabled = true
    expect(isDetailDirty(d, c)).toBe(false)
  })
  it('ignores a freshly added blank header row', () => {
    const d = detail()
    const c = cloneDetail(d)
    c.customHeaders.push({ key: '', value: '', secret: false })
    expect(isDetailDirty(d, c)).toBe(false)
    c.customHeaders[0].key = 'X-Test'
    expect(isDetailDirty(d, c)).toBe(true)
  })
})
