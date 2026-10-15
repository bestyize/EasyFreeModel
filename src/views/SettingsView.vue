<script setup lang="ts">
import { computed, onActivated, onBeforeUnmount, onMounted, ref } from 'vue'
import { onBeforeRouteLeave } from 'vue-router'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import SettingsAboutSection from '@/components/settings/SettingsAboutSection.vue'
import SettingsAppearanceSection from '@/components/settings/SettingsAppearanceSection.vue'
import SettingsDataSection from '@/components/settings/SettingsDataSection.vue'
import SettingsRoutingSection from '@/components/settings/SettingsRoutingSection.vue'
import SettingsServiceSection from '@/components/settings/SettingsServiceSection.vue'
import { isLoopbackAddress } from '@/components/settings/address'
import { validateSettings } from '@/components/settings/validation'
import { useSettingsStore } from '@/stores/settings'
import { useSystemStore } from '@/stores/system'

const settings = useSettingsStore()
const system = useSystemStore()

const exposeOpen = ref(false)
const leaveOpen = ref(false)
const savedNote = ref(false)
let leaveResolve: ((ok: boolean) => void) | null = null

const errors = computed(() => (settings.draft ? validateSettings(settings.draft) : {}))
const hasErrors = computed(() => Object.keys(errors.value).length > 0)

async function doSave() {
  const d = settings.draft
  if (!d) return
  d.listenAddress = d.listenAddress.trim()
  savedNote.value = false
  if (await settings.save()) {
    savedNote.value = true
    void system.refresh()
  }
}

function onSave() {
  const d = settings.draft
  const s = settings.saved
  if (!d || !s || hasErrors.value || settings.saving) return
  const changed = d.listenAddress.trim() !== s.listenAddress
  if (changed && !isLoopbackAddress(d.listenAddress)) {
    exposeOpen.value = true
    return
  }
  void doSave()
}

function onReset() {
  savedNote.value = false
  settings.reset()
}

// 未保存修改：路由离开前确认
onBeforeRouteLeave(() => {
  if (!settings.dirty) return true
  leaveOpen.value = true
  return new Promise<boolean>((resolve) => {
    leaveResolve = resolve
  })
})
function settleLeave(ok: boolean) {
  leaveResolve?.(ok)
  leaveResolve = null
}
function onLeaveConfirm() {
  settleLeave(true)
}
function onLeaveModelUpdate(v: boolean) {
  leaveOpen.value = v
  if (!v) settleLeave(false) // 取消 / ESC：留在当前页（确认时 confirm 先于此触发，resolve 已为 true）
}

function onBeforeUnload(e: BeforeUnloadEvent) {
  if (settings.dirty) {
    e.preventDefault()
    e.returnValue = ''
  }
}

let activatedOnce = false
onMounted(() => {
  void settings.ensureLoaded()
  window.addEventListener('beforeunload', onBeforeUnload)
})
onActivated(() => {
  if (!activatedOnce) {
    activatedOnce = true
    return
  }
  void settings.load() // 有未保存草稿时 store 不会覆盖
})
onBeforeUnmount(() => window.removeEventListener('beforeunload', onBeforeUnload))
</script>

<template>
  <div class="page">
    <p v-if="settings.loadError" class="banner error" role="alert">
      加载设置失败：{{ settings.loadError }}
      <button class="btn" type="button" @click="settings.load(true)">重试</button>
    </p>
    <p v-else-if="!settings.draft" class="muted">加载中…</p>

    <template v-if="settings.draft">
      <SettingsServiceSection :errors="errors" />
      <SettingsRoutingSection :errors="errors" />
      <SettingsDataSection />
      <SettingsAppearanceSection />
      <SettingsAboutSection />
    </template>

    <div v-if="settings.draft" class="bar" data-testid="save-bar">
      <span v-if="settings.saveError" class="msg error" role="alert">保存失败：{{ settings.saveError }}</span>
      <span v-else-if="settings.dirty" class="msg warn" role="status" data-testid="dirty-note">有未保存的修改</span>
      <span v-else-if="savedNote" class="msg ok" role="status">已保存</span>
      <button class="btn" type="button" data-testid="reset" :disabled="!settings.dirty || settings.saving" @click="onReset">重置</button>
      <button
        class="btn btn-primary" type="button" data-testid="save"
        :disabled="!settings.dirty || hasErrors || settings.saving" @click="onSave"
      >
        {{ settings.saving ? '保存中…' : '保存' }}
      </button>
    </div>

    <ConfirmDialog
      v-model="exposeOpen"
      title="允许非本机访问？"
      :message="`监听地址「${settings.draft?.listenAddress.trim()}」不是回环地址，局域网或外网设备将能访问本地网关。必须在「API 密钥」页创建并要求使用密钥认证，否则任何人都可能消耗你的上游额度。确定保存吗？`"
      confirm-text="确认并保存"
      danger
      @confirm="doSave"
    />
    <ConfirmDialog
      :model-value="leaveOpen"
      title="放弃未保存的修改？"
      message="设置有未保存的修改，离开后这些修改会保留在草稿中但不会生效。确定离开吗？"
      confirm-text="离开"
      cancel-text="留在此页"
      @confirm="onLeaveConfirm"
      @update:model-value="onLeaveModelUpdate"
    />
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; gap: 16px; padding-bottom: 72px; max-width: 880px; }
.muted { color: var(--c-text-2); }
.banner { display: flex; align-items: center; justify-content: space-between; margin: 0; padding: 12px 14px; border-radius: var(--radius); }
.banner.error { background: var(--c-danger-soft); color: var(--c-danger); }
.bar { position: sticky; bottom: 0; display: flex; align-items: center; justify-content: flex-end; gap: 10px; padding: 12px 0; background: var(--c-canvas); border-top: 1px solid var(--c-border); }
.msg { margin-right: auto; font-size: var(--fs-small); }
.msg.warn { color: var(--c-warning); }
.msg.error { color: var(--c-danger); }
.msg.ok { color: var(--c-success); }
</style>
