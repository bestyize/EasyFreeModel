import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import type { Connection } from '@/types'
import ConnectionRow from './ConnectionRow.vue'
import { connectionStatus } from './status'

const conn = (over: Partial<Connection> = {}): Connection => ({
  id: 'c1', providerId: 'claude', accountLabel: 'me@example.com', kind: 'oauth', status: 'active', priority: 1, weight: 1, enabled: true, ...over,
})

function row(c: Connection, extra: Record<string, unknown> = {}) {
  return mount(ConnectionRow, { props: { connection: c, index: 0, selected: false, expanded: false, ...extra } })
}

describe('connectionStatus', () => {
  it('needsReauth 是警告色的“需要重新授权”，即使已启用也不是“活跃”', () => {
    expect(connectionStatus({ status: 'needsReauth', enabled: true })).toEqual({ text: '需要重新授权', tone: 'warning' })
    expect(connectionStatus({ status: 'needsReauth', enabled: false }).text).toBe('需要重新授权')
    expect(connectionStatus({ status: 'active', enabled: true })).toEqual({ text: '活跃', tone: 'success' })
    expect(connectionStatus({ status: 'active', enabled: true }, true).text).toBe('待授权')
  })
})

describe('ConnectionRow：OAuth', () => {
  it('显示 OAuth 标签；正常连接没有“重新授权”按钮', () => {
    const w = row(conn())
    expect(w.find('.chip').text()).toBe('OAuth')
    expect(w.text()).toContain('活跃')
    expect(w.text()).not.toContain('重新授权')
  })

  it('needsReauth：警告色状态文案 + 可见的“重新授权”按钮，点击发出 reauthorize', async () => {
    const w = row(conn({ status: 'needsReauth' }))
    expect(w.text()).toContain('需要重新授权')
    expect(w.text()).not.toContain('活跃')
    expect(w.find('.tone-warning').exists()).toBe(true)
    const b = w.find('button[aria-label="重新授权 me@example.com"]')
    expect(b.text()).toContain('重新授权')
    await b.trigger('click')
    expect(w.emitted('reauthorize')).toHaveLength(1)
  })

  it('“删除”变成“断开”，发出 disconnect 而不是 remove；只读（适配器未就绪）时仍可断开', async () => {
    const w = row(conn(), { readonly: true })
    expect(w.find('button[aria-label="删除连接 me@example.com"]').exists()).toBe(false)
    const b = w.find('button[aria-label="断开连接 me@example.com"]')
    expect(b.text()).toContain('断开')
    expect(b.attributes('disabled')).toBeUndefined()
    await b.trigger('click')
    expect(w.emitted('disconnect')).toHaveLength(1)
    expect(w.emitted('remove')).toBeUndefined()
  })

  it('oauthBusy 时重新授权/断开按钮禁用', () => {
    const w = row(conn({ status: 'needsReauth' }), { oauthBusy: true })
    expect(w.find('button[aria-label="重新授权 me@example.com"]').attributes('disabled')).toBeDefined()
    expect(w.find('button[aria-label="断开连接 me@example.com"]').attributes('disabled')).toBeDefined()
  })

  it('测试按钮仍然保留（由后端如实回答）', () => {
    const w = row(conn())
    expect(w.find('button[aria-label="测试连接 me@example.com"]').exists()).toBe(true)
  })

  it('API Key 连接保持“删除”，不出现断开 / 重新授权', async () => {
    const w = row(conn({ kind: 'apiKey' }))
    expect(w.find('.chip').text()).toBe('API Key')
    expect(w.find('button[aria-label="断开连接 me@example.com"]').exists()).toBe(false)
    await w.find('button[aria-label="删除连接 me@example.com"]').trigger('click')
    expect(w.emitted('remove')).toHaveLength(1)
  })
})
