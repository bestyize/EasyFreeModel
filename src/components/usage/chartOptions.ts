import type { EChartsCoreOption } from 'echarts/core'
import type { UsageSummary } from '@/types'
import { colorFor, formatCompact, formatCost, formatInt } from './format'

export type TrendMetric = 'token' | 'requests' | 'cost'

export const TREND_METRICS: { value: TrendMetric; label: string }[] = [
  { value: 'token', label: 'Token' },
  { value: 'requests', label: '请求' },
  { value: 'cost', label: '成本' },
]

export const SERIES_COLORS = {
  input: '#ff2442',
  output: '#3b82f6',
  cached: '#8b5cf6',
  requests: '#ff2442',
  cost: '#f4a524',
} as const

type TrendPoint = UsageSummary['trend'][number]

interface SeriesDef {
  name: string
  color: string
  pick: (p: TrendPoint) => number
}

/** Token 视图并列展示 输入/输出/缓存 三条线，不做任何相加 */
export function seriesFor(metric: TrendMetric): SeriesDef[] {
  switch (metric) {
    case 'token':
      return [
        { name: '输入 Token', color: SERIES_COLORS.input, pick: (p) => p.input },
        { name: '输出 Token', color: SERIES_COLORS.output, pick: (p) => p.output },
        { name: '缓存 Token', color: SERIES_COLORS.cached, pick: (p) => p.cached },
      ]
    case 'requests':
      return [{ name: '请求数', color: SERIES_COLORS.requests, pick: (p) => p.requests }]
    case 'cost':
      return [{ name: '估算成本', color: SERIES_COLORS.cost, pick: (p) => p.cost }]
  }
}

export function formatMetricValue(metric: TrendMetric, v: number): string {
  return metric === 'cost' ? formatCost(v) : formatInt(v)
}

/** 折线图选项：纯色描边 + 低透明度纯色填充，不使用渐变 */
export function buildTrendOption(
  trend: UsageSummary['trend'],
  metric: TrendMetric,
  hidden: readonly string[] = [],
  dark = false,
): EChartsCoreOption {
  const defs = seriesFor(metric)
  const muted = dark ? '#b3b3b3' : '#677084'
  const border = dark ? '#393939' : '#e7e9ed'
  return {
    animation: false,
    color: defs.map((d) => d.color),
    grid: { left: 8, right: 16, top: 16, bottom: 4, containLabel: true },
    // 图例由外部 HTML 渲染（可点击），这里只用 legend 组件承载选中状态
    legend: {
      show: false,
      data: defs.map((d) => d.name),
      selected: Object.fromEntries(defs.map((d) => [d.name, !hidden.includes(d.name)])),
    },
    tooltip: {
      trigger: 'axis',
      backgroundColor: dark ? '#292929' : '#ffffff',
      borderColor: border,
      textStyle: { color: dark ? '#f2f2f2' : '#16181d' },
      valueFormatter: (v: unknown) => formatMetricValue(metric, Number(v)),
    },
    xAxis: {
      type: 'category',
      boundaryGap: false,
      data: trend.map((p) => p.label),
      axisLine: { lineStyle: { color: border } },
      axisTick: { show: false },
      axisLabel: { color: muted, hideOverlap: true },
    },
    yAxis: {
      type: 'value',
      axisLabel: {
        color: muted,
        formatter: (v: number) => (metric === 'cost' ? formatCost(v) : formatCompact(v)),
      },
      splitLine: { lineStyle: { color: border } },
    },
    series: defs.map((d) => ({
      name: d.name,
      type: 'line',
      smooth: 0.25,
      showSymbol: trend.length <= 2,
      symbolSize: 6,
      lineStyle: { width: 2, color: d.color },
      itemStyle: { color: d.color },
      areaStyle: { color: d.color, opacity: 0.07 },
      data: trend.map(d.pick),
    })),
  }
}

export function buildDonutOption(items: UsageSummary['providerShare'], dark = false): EChartsCoreOption {
  return {
    animation: false,
    tooltip: {
      trigger: 'item',
      backgroundColor: dark ? '#292929' : '#ffffff',
      borderColor: dark ? '#393939' : '#e7e9ed',
      textStyle: { color: dark ? '#f2f2f2' : '#16181d' },
      formatter: (p: { name: string; value: number; percent: number }) =>
        `${p.name}<br/>${formatInt(p.value)} Token · ${p.percent.toFixed(1)}%`,
    },
    series: [
      {
        type: 'pie',
        radius: ['62%', '90%'],
        center: ['50%', '50%'],
        avoidLabelOverlap: true,
        label: { show: false },
        labelLine: { show: false },
        itemStyle: { borderColor: dark ? '#191919' : '#ffffff', borderWidth: 2 },
        data: items.map((i) => ({
          name: i.name,
          value: i.tokens,
          itemStyle: { color: colorFor(i.providerId) },
        })),
      },
    ],
  }
}
