import { describe, expect, it } from 'vitest'
import { compareLabel, formatCompact, formatCost, formatDelta, formatRelative } from './format'

describe('format', () => {
  it('formatCompact', () => {
    expect(formatCompact(999)).toBe('999')
    expect(formatCompact(1500)).toBe('1.5K')
    expect(formatCompact(62_400_000)).toBe('62.4M')
    expect(formatCompact(2_000_000)).toBe('2M')
  })

  it('formatCost 极小金额不显示成 $0.00', () => {
    expect(formatCost(0)).toBe('$0.00')
    expect(formatCost(0.0012)).toBe('$0.0012')
    expect(formatCost(0.29)).toBe('$0.29')
  })

  it('formatDelta', () => {
    expect(formatDelta(12.4)).toBe('+12%')
    expect(formatDelta(-18)).toBe('-18%')
  })

  it('compareLabel：all 没有对比文案', () => {
    expect(compareLabel('today')).toBe('较昨日')
    expect(compareLabel('all')).toBe('')
  })

  it('formatRelative', () => {
    const now = Date.parse('2026-10-14T12:00:00Z')
    expect(formatRelative('2026-10-14T11:55:00Z', now)).toBe('5 分钟前')
    expect(formatRelative(null, now)).toBe('—')
    expect(formatRelative('not-a-date', now)).toBe('—')
  })
})
