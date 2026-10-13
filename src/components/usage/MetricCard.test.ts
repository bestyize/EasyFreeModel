import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import { FileText } from 'lucide-vue-next'
import MetricCard from './MetricCard.vue'

const mk = (props: Record<string, unknown>) =>
  mount(MetricCard, { props: { label: '请求总数', value: '210', icon: FileText, deltaPct: null, ...props } })

describe('MetricCard', () => {
  it('deltaPct 为 null 时不显示涨跌', () => {
    const w = mk({ deltaPct: null, compareText: '较昨日' })
    expect(w.find('[data-test="delta"]').exists()).toBe(false)
    expect(w.text()).not.toContain('%')
    expect(w.text()).not.toContain('较昨日')
  })

  it('正数显示 +12%，负数显示 -18%', () => {
    expect(mk({ deltaPct: 12.3, compareText: '较昨日' }).find('[data-test="delta"]').text()).toContain('+12%')
    expect(mk({ deltaPct: -18, compareText: '较昨日' }).find('[data-test="delta"]').text()).toContain('-18%')
  })

  it('deltaPct 为 0 时显示 0%（与 null 不同）', () => {
    const w = mk({ deltaPct: 0 })
    expect(w.find('[data-test="delta"]').text()).toContain('0%')
  })

  it('成本类指标（positiveIsGood=false）上升为 bad，下降为 good', () => {
    expect(mk({ deltaPct: 10, positiveIsGood: false }).find('[data-test="delta"]').classes()).toContain('tone-bad')
    expect(mk({ deltaPct: -10, positiveIsGood: false }).find('[data-test="delta"]').classes()).toContain('tone-good')
    expect(mk({ deltaPct: 10 }).find('[data-test="delta"]').classes()).toContain('tone-good')
  })

  it('note 原样展示（用于成本估算来源）', () => {
    const w = mk({ note: '估算 · 公开价目表' })
    expect(w.find('[data-test="note"]').text()).toBe('估算 · 公开价目表')
  })
})
