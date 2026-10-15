import { computed } from 'vue'
import { useSettingsStore } from '@/stores/settings'
import { useSystemStore } from '@/stores/system'
import { gatewayOrigin, isWildcardAddress } from './snippets'

/** 客户端该连的网关地址：运行中以实际监听为准，否则用设置里保存的值。 */
export function useGatewayAddress() {
  const settings = useSettingsStore()
  const system = useSystemStore()

  const running = computed(() => system.status?.gateway.running ?? false)
  const listen = computed(() => {
    const g = system.status?.gateway
    if (g?.running) return { address: g.address, port: g.port }
    const s = settings.saved
    return s ? { address: s.listenAddress, port: s.port } : null
  })
  const origin = computed(() => (listen.value ? gatewayOrigin(listen.value.address, listen.value.port) : ''))
  const wildcard = computed(() => isWildcardAddress(listen.value?.address))

  return { running, listen, origin, wildcard }
}
