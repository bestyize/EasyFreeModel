import { describe, expect, it } from 'vitest'
import type { UsageSummary } from '@/types'
import { buildDonutOption, buildTrendOption, seriesFor } from './chartOptions'

const trend: UsageSummary['trend'] = [
  { label: '00:00', input: 100, output: 10, cached: 500, requests: 3, cost: 0.1 },
  { label: '01:00', input: 200, output: 20, cached: 0, requests: 4, cost: 0.2 },
]

interface Opt {
  series: { name: string; data: number[]; areaStyle: { color: string; opacity: number } }[]
  legend: { selected: Record<string, boolean> }
}

describe('buildTrendOption', () => {
  it('Token 视图：输入/输出/缓存是三条独立折线，没有相加', () => {
    const o = buildTrendOption(trend, 'token') as unknown as Opt
    expect(o.series.map((s) => s.name)).toEqual(['输入 Token', '输出 Token', '缓存 Token'])
    expect(o.series[0]!.data).toEqual([100, 200])
    expect(o.series[2]!.data).toEqual([500, 0])
  })

  it('请求/成本视图各一条线', () => {
    expect(seriesFor('requests')).toHaveLength(1)
    expect((buildTrendOption(trend, 'cost') as unknown as Opt).series[0]!.data).toEqual([0.1, 0.2])
  })

  it('图例隐藏状态反映到 legend.selected', () => {
    const o = buildTrendOption(trend, 'token', ['输出 Token']) as unknown as Opt
    expect(o.legend.selected).toEqual({ '输入 Token': true, '输出 Token': false, '缓存 Token': true })
  })

  it('面积填充是纯色 + 低透明度，不是渐变对象', () => {
    const o = buildTrendOption(trend, 'token') as unknown as Opt
    for (const s of o.series) {
      expect(typeof s.areaStyle.color).toBe('string')
      expect(s.areaStyle.opacity).toBeLessThanOrEqual(0.1)
    }
  })

  it('夜间模式的坐标轴、提示框与圆环边框使用深色配色', () => {
    const trendOption = buildTrendOption(trend, 'token', [], true) as unknown as {
      xAxis: { axisLabel: { color: string } }; tooltip: { backgroundColor: string }
    }
    expect(trendOption.xAxis.axisLabel.color).toBe('#b3b3b3')
    expect(trendOption.tooltip.backgroundColor).toBe('#292929')
    const donut = buildDonutOption([], true) as unknown as { series: { itemStyle: { borderColor: string } }[] }
    expect(donut.series[0]!.itemStyle.borderColor).toBe('#191919')
  })
})
