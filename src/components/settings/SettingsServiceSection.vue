<script setup lang="ts">
import { computed, ref } from 'vue'
import { Play, Square, TriangleAlert } from 'lucide-vue-next'
import StatusTag from '@/components/StatusTag.vue'
import { errorMessage } from '@/components/media/format'
import { useSettingsStore } from '@/stores/settings'
import { useSystemStore } from '@/stores/system'
import { isLoopbackAddress } from './address'
import SettingsRow from './SettingsRow.vue'
import ToggleSwitch from './ToggleSwitch.vue'
import type { SettingsErrors } from './validation'

defineProps<{ errors: SettingsErrors }>()

const settings = useSettingsStore()
const system = useSystemStore()
const toggling = ref(false)
const toggleError = ref<string | null>(null)

const gateway = computed(() => system.status?.gateway ?? null)
const draft = computed(() => settings.draft!)
const exposed = computed(() => !isLoopbackAddress(draft.value.listenAddress))

/** 运行中的服务与已保存配置不一致：改动需要重启才会生效 */
const needsRestart = computed(() => {
  const g = gateway.value
  const s = settings.saved
  return !!g && g.running && !!s && (g.address !== s.listenAddress || g.port !== s.port)
})

async function toggle() {
  if (toggling.value) return
  toggling.value = true
  toggleError.value = null
  try {
    await system.toggleGateway()
  } catch (e) {
    toggleError.value = errorMessage(e)
  } finally {
    toggling.value = false
  }
}
</script>

<template>
  <section class="card section" aria-labelledby="set-service">
    <h2 id="set-service" class="section-title">本地服务</h2>

    <div class="gateway" data-testid="gateway-box">
      <div class="gw-info">
        <template v-if="gateway">
          <StatusTag :tone="gateway.running ? 'success' : 'neutral'" data-testid="gateway-state">
            {{ gateway.running ? '运行中' : '已停止' }}
          </StatusTag>
          <span class="addr">{{ gateway.address }}:{{ gateway.port }}</span>
        </template>
        <StatusTag v-else tone="warning" data-testid="gateway-state">状态未知</StatusTag>
      </div>
      <button
        class="btn" :class="{ 'btn-primary': gateway && !gateway.running }" type="button"
        data-testid="gateway-toggle" :disabled="!gateway || toggling" @click="toggle"
      >
        <component :is="gateway?.running ? Square : Play" :size="14" aria-hidden="true" />
        {{ toggling ? '处理中…' : gateway?.running ? '停止服务' : '启动服务' }}
      </button>
    </div>
    <p v-if="gateway?.error" class="alert" role="alert" data-testid="gateway-error">
      <TriangleAlert :size="16" aria-hidden="true" /> 服务错误：{{ gateway.error }}
    </p>
    <p v-if="toggleError" class="alert" role="alert">操作失败：{{ toggleError }}</p>
    <p v-if="system.loadError" class="alert" role="alert">无法获取服务状态：{{ system.loadError }}</p>
    <p v-if="settings.dirty" class="note">配置有未保存的修改，启动服务将使用已保存的配置。</p>
    <p v-if="needsRestart" class="note" data-testid="restart-note">
      运行中的服务仍监听 {{ gateway?.address }}:{{ gateway?.port }}，已保存的新地址/端口需重启服务后生效。
    </p>

    <SettingsRow label="监听地址" description="默认 127.0.0.1，仅本机可访问。" :error="errors.listenAddress" for-id="s-addr">
      <input id="s-addr" v-model="draft.listenAddress" class="input" autocomplete="off" spellcheck="false" />
      <template #extra>
        <p v-if="exposed && !errors.listenAddress" class="risk" data-testid="expose-warning">
          非回环地址会让局域网 / 外网设备访问网关，必须配合 API 密钥认证使用。
        </p>
      </template>
    </SettingsRow>
    <SettingsRow label="端口" description="1–65535。端口被占用时，启动会在上方显示错误。" :error="errors.port" for-id="s-port">
      <input id="s-port" v-model.number="draft.port" class="input narrow" type="number" min="1" max="65535" />
    </SettingsRow>
    <SettingsRow label="开机自启" description="登录系统后自动启动本应用。">
      <ToggleSwitch v-model="draft.autoStart" label="开机自启" />
    </SettingsRow>
    <SettingsRow label="托盘运行" description="关闭窗口后保持在系统托盘继续运行。">
      <ToggleSwitch v-model="draft.trayEnabled" label="托盘运行" />
    </SettingsRow>
  </section>
</template>

<style scoped>
.section { padding: 20px 24px; }
.gateway { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin: 12px 0 4px; padding: 12px 14px; border-radius: 8px; background: var(--c-sidebar); }
.gw-info { display: flex; align-items: center; gap: 10px; }
.addr { font-family: var(--font-mono); font-size: var(--fs-small); }
.alert { display: flex; align-items: center; gap: 8px; margin: 8px 0 0; padding: 10px 12px; border-radius: 8px; background: var(--c-danger-soft); color: var(--c-danger); font-size: var(--fs-small); }
.note { margin: 8px 0 0; color: var(--c-warning); font-size: var(--fs-small); }
.risk { margin: 6px 0 0; color: var(--c-danger); font-size: var(--fs-caption); }
.narrow { width: 120px; }
</style>
