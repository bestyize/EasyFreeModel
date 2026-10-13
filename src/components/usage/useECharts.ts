import { onBeforeUnmount, onMounted, shallowRef, watch, type Ref } from 'vue'
import * as echarts from 'echarts/core'
import type { EChartsCoreOption, ECharts } from 'echarts/core'
import { LineChart, PieChart } from 'echarts/charts'
import { GridComponent, LegendComponent, TooltipComponent } from 'echarts/components'
import { CanvasRenderer } from 'echarts/renderers'

// 按需引入，避免把整个 echarts 打进包里
echarts.use([LineChart, PieChart, GridComponent, LegendComponent, TooltipComponent, CanvasRenderer])

/**
 * 把 echarts 实例的生命周期绑到组件：挂载时 init、选项变化时 setOption、
 * 容器尺寸变化时 resize、卸载时 dispose。
 *
 * resetOn 的值变化时整体替换选项（例如切换指标，需要重置图例选中状态）；
 * 其余情况仅替换 series，从而保留用户点击图例隐藏的系列。
 */
export function useECharts(
  el: Ref<HTMLElement | null>,
  getOption: () => EChartsCoreOption,
  resetOn?: () => unknown,
) {
  const chart = shallowRef<ECharts | null>(null)
  let observer: ResizeObserver | undefined
  let lastReset = resetOn?.()

  function apply() {
    if (!chart.value) return
    const r = resetOn?.()
    const reset = r !== lastReset
    lastReset = r
    chart.value.setOption(getOption(), reset ? { notMerge: true } : { replaceMerge: ['series'] })
  }

  onMounted(() => {
    if (!el.value) return
    chart.value = echarts.init(el.value, undefined, { renderer: 'canvas' })
    apply()
    if (typeof ResizeObserver !== 'undefined') {
      observer = new ResizeObserver(() => chart.value?.resize())
      observer.observe(el.value)
    }
  })

  watch(getOption, apply)

  onBeforeUnmount(() => {
    observer?.disconnect()
    chart.value?.dispose()
    chart.value = null
  })

  return { chart }
}
