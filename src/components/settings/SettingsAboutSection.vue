<script setup lang="ts">
import { computed, ref } from 'vue'
import { APP_NAME, APP_VERSION } from '@/router/nav'
import { useSystemStore } from '@/stores/system'
import SettingsRow from './SettingsRow.vue'

const system = useSystemStore()
const copyMsg = ref<string | null>(null)
const inTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

const diagnostics = computed(() => {
  const g = system.status?.gateway
  return [
    `${APP_NAME} ${APP_VERSION}`,
    `运行环境: ${inTauri ? 'Tauri 桌面' : '浏览器预览（mock 数据）'}`,
    `系统: ${typeof navigator !== 'undefined' ? navigator.userAgent : '未知'}`,
    `本地服务: ${g ? `${g.running ? '运行中' : '已停止'} ${g.address}:${g.port}` : '状态未知'}`,
    `服务错误: ${g?.error ?? '无'}`,
  ].join('\n')
})

async function copy() {
  try {
    await navigator.clipboard.writeText(diagnostics.value)
    copyMsg.value = '已复制诊断信息'
  } catch {
    copyMsg.value = '复制失败，请手动选中文本复制'
  }
}
</script>

<template>
  <section class="card section" aria-labelledby="set-about">
    <h2 id="set-about" class="section-title">关于</h2>
    <SettingsRow label="版本"><span data-testid="app-version">{{ APP_VERSION }}</span></SettingsRow>
    <SettingsRow label="许可" description="项目当前未包含 LICENSE 文件，尚未声明开源许可证。">
      <span class="muted">未声明</span>
    </SettingsRow>
    <SettingsRow label="检查更新" description="自动更新属于后续里程碑（M5）。">
      <span class="pending">待实现</span>
      <button class="btn" type="button" disabled title="待实现：自动更新尚未接入">检查更新</button>
    </SettingsRow>
    <div class="diag">
      <div class="diag-head">
        <h3>诊断信息</h3>
        <button class="btn" type="button" @click="copy">复制</button>
      </div>
      <pre>{{ diagnostics }}</pre>
      <p v-if="copyMsg" class="muted" role="status">{{ copyMsg }}</p>
    </div>
  </section>
</template>

<style scoped>
.section { padding: 20px 24px; }
.muted { color: var(--c-text-2); font-size: var(--fs-small); }
.pending { padding: 2px 8px; border-radius: 4px; background: var(--c-surface-2); color: var(--c-text-2); font-size: var(--fs-caption); }
.diag { padding-top: 14px; border-top: 1px solid var(--c-border); }
.diag-head { display: flex; align-items: center; justify-content: space-between; }
h3 { margin: 0; font-size: var(--fs-body); font-weight: 500; }
pre { margin: 10px 0 0; padding: 12px; border-radius: 8px; background: var(--c-surface-2); font: var(--fs-caption) var(--font-mono); white-space: pre-wrap; word-break: break-all; user-select: text; }
</style>
