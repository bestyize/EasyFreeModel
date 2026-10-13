import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import ProviderCard from './ProviderCard.vue'
import type { ProviderSummary } from '@/types'

function summary(over: Partial<ProviderSummary> = {}): ProviderSummary {
  return {
    id: 'x',
    name: 'Kimi',
    group: 'oauth',
    status: 'disconnected',
    connectionCount: 0,
    adapterReady: false,
    protocols: [],
    quotaHint: null,
    serviceKinds: ['llm'],
    website: null,
    color: null,
    textIcon: null,
    authModes: ['apiKey'],
    pendingReason: null,
    ...over,
  }
}

describe('ProviderCard', () => {
  it('shows 待接入 when adapterReady=false, even if status claims connected', () => {
    const w = mount(ProviderCard, { props: { provider: summary({ status: 'connected', connectionCount: 3 }) } })
    expect(w.text()).toContain('待接入')
    expect(w.text()).not.toContain('已连接')
  })

  it('ignores batch-test overlay for adapters that are not ready', () => {
    const w = mount(ProviderCard, {
      props: { provider: summary(), batch: { phase: 'done', ok: 1, total: 1 } },
    })
    expect(w.text()).toContain('待接入')
    expect(w.text()).not.toContain('测试通过')
  })

  it('shows real status when adapter is ready', () => {
    const connected = mount(ProviderCard, { props: { provider: summary({ adapterReady: true, status: 'connected' }) } })
    expect(connected.text()).toContain('已连接')
    const off = mount(ProviderCard, { props: { provider: summary({ adapterReady: true }) } })
    expect(off.text()).toContain('未连接')
  })

  it('emits open on click and is keyboard-reachable (native button)', async () => {
    const p = summary()
    const w = mount(ProviderCard, { props: { provider: p } })
    expect(w.element.tagName).toBe('BUTTON')
    expect(w.attributes('aria-label')).toContain('Kimi')
    await w.trigger('click')
    expect(w.emitted('open')?.[0]).toEqual([p])
  })
})
