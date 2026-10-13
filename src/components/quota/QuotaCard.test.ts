import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import type { QuotaAccount, QuotaPeriod } from '@/types'
import QuotaCard from './QuotaCard.vue'
import QuotaBar from './QuotaBar.vue'
import QuotaPeriodRow from './QuotaPeriodRow.vue'

const NOW = Date.parse('2026-10-14T12:00:00Z')

const period = (over: Partial<QuotaPeriod> = {}): QuotaPeriod => ({
  name: '5h',
  unit: '',
  used: 21,
  total: 100,
  remaining: 79,
  resetsAt: new Date(NOW + 3600_000).toISOString(),
  source: 'live',
  ...over,
})

const acct = (over: Partial<QuotaAccount> = {}): QuotaAccount => ({
  connectionId: 'c1',
  providerId: 'openai-codex',
  providerName: 'OpenAI Codex',
  accountLabel: 'me@example.com',
  enabled: true,
  periods: [period()],
  balances: [],
  lastSyncedAt: new Date(NOW - 5 * 60_000).toISOString(),
  error: null,
  ...over,
})

const mountCard = (account: QuotaAccount) => mount(QuotaCard, { props: { account, now: NOW } })

describe('QuotaPeriodRow', () => {
  it('未知值显示“—”与“未知”，绝不显示 0%，且没有进度填充', () => {
    const w = mount(QuotaPeriodRow, {
      props: { period: period({ used: null, total: null, remaining: null, resetsAt: null, source: 'unknown' }), now: NOW },
    })
    expect(w.get('[data-test="pct"]').text()).toBe('未知')
    expect(w.get('[data-test="usage"]').text()).toBe('— / —')
    expect(w.get('[data-test="reset"]').text()).toBe('重置时间未知')
    expect(w.text()).not.toContain('0%')
    expect(w.find('[data-test="fill"]').exists()).toBe(false)
    expect(w.classes()).toContain('lv-unknown')
  })

  it('已知为 0 显示“0%”、红色、“0 / 100”', () => {
    const w = mount(QuotaPeriodRow, {
      props: { period: period({ used: 100, total: 100, remaining: 0 }), now: NOW },
    })
    expect(w.get('[data-test="pct"]').text()).toBe('0%')
    expect(w.get('[data-test="usage"]').text()).toBe('100 / 100')
    expect(w.classes()).toContain('lv-danger')
  })

  it('颜色随剩余百分比：79% 绿、20% 黄、5% 红', () => {
    const lv = (remaining: number) =>
      mount(QuotaPeriodRow, { props: { period: period({ remaining, used: 100 - remaining }), now: NOW } }).classes()
    expect(lv(79)).toContain('lv-ok')
    expect(lv(21)).toContain('lv-ok')
    expect(lv(20)).toContain('lv-warn')
    expect(lv(6)).toContain('lv-warn')
    expect(lv(5)).toContain('lv-danger')
  })

  it('保留上游周期原名，显示倒计时', () => {
    const w = mount(QuotaPeriodRow, { props: { period: period({ name: 'Bonus Pack 1' }), now: NOW } })
    expect(w.text()).toContain('Bonus Pack 1')
    expect(w.get('[data-test="reset"]').text()).toBe('1h 0m 后重置')
  })

  it('source=estimated / cached 有小标注，live 没有', () => {
    const src = (source: QuotaPeriod['source']) =>
      mount(QuotaPeriodRow, { props: { period: period({ source }), now: NOW } }).find('[data-test="source"]')
    expect(src('estimated').text()).toBe('估算')
    expect(src('cached').text()).toBe('缓存')
    expect(src('live').exists()).toBe(false)
  })
})

describe('QuotaBar', () => {
  it('填充宽度等于剩余百分比', () => {
    const w = mount(QuotaBar, { props: { pct: 65.4 } })
    expect(w.get('[data-test="fill"]').attributes('style')).toContain('width: 65.4%')
    expect(w.attributes('aria-valuenow')).toBe('65.4')
  })
  it('未知时 aria-valuetext 为“未知”且无 aria-valuenow', () => {
    const w = mount(QuotaBar, { props: { pct: null } })
    expect(w.attributes('aria-valuetext')).toBe('未知')
    expect(w.attributes('aria-valuenow')).toBeUndefined()
  })
})

describe('QuotaCard', () => {
  it('只有货币余额的账户显示余额，不伪造百分比和进度条', () => {
    const w = mountCard(
      acct({ periods: [], balances: [{ currency: 'USD', amount: 0 }, { currency: 'CNY', amount: 55.7 }, { currency: 'EUR', amount: null }] }),
    )
    const items = w.findAll('[data-test="balance"]').map((b) => b.text())
    expect(items[0]).toContain('0.00 USD')
    expect(items[1]).toContain('55.70 CNY')
    expect(items[2]).toContain('未知')
    expect(w.find('[data-test="period"]').exists()).toBe(false)
    expect(w.find('[role="progressbar"]').exists()).toBe(false)
    expect(w.text()).not.toContain('%')
  })

  it('刷新失败：显示 error，同时保留上次成功同步时间和旧数据', () => {
    const w = mountCard(acct({ error: '401 expired', lastSyncedAt: new Date(NOW - 5 * 3600_000).toISOString() }))
    expect(w.get('[data-test="error"]').text()).toContain('401 expired')
    expect(w.get('[data-test="synced"]').text()).toContain('上次成功同步 5 小时前')
    expect(w.find('[data-test="period"]').exists()).toBe(true)
  })

  it('从未同步成功：显示“尚未同步成功”', () => {
    const w = mountCard(acct({ lastSyncedAt: null }))
    expect(w.get('[data-test="synced"]').text()).toBe('尚未同步成功')
  })

  it('周期很多时默认折叠，可展开', async () => {
    const periods = Array.from({ length: 11 }, (_, i) => period({ name: `Pack ${i + 1}` }))
    const w = mountCard(acct({ periods }))
    expect(w.findAll('[data-test="period"]')).toHaveLength(5)
    await w.get('button.more').trigger('click')
    expect(w.findAll('[data-test="period"]')).toHaveLength(11)
  })

  it('无任何额度数据时给出提示而非 0', () => {
    const w = mountCard(acct({ periods: [], balances: [] }))
    expect(w.text()).toContain('暂无额度数据')
  })

  it('开关与同步按钮发出事件', async () => {
    const w = mountCard(acct())
    await w.get('[role="switch"]').trigger('click')
    expect(w.emitted('toggle')![0]).toEqual(['c1', false])
    await w.get('[data-test="sync"]').trigger('click')
    expect(w.emitted('sync')![0]).toEqual(['c1'])
  })

  it('停用账户显示“已停用”', () => {
    expect(mountCard(acct({ enabled: false })).text()).toContain('已停用')
  })
})
