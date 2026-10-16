<script setup lang="ts">
import { onBeforeUnmount, onMounted } from 'vue'
import { RouterView } from 'vue-router'
import Sidebar from './Sidebar.vue'
import PageHeader from './PageHeader.vue'
import UpdateNotice from './UpdateNotice.vue'
import { useUiStore } from '@/stores/ui'
import { useSystemStore } from '@/stores/system'
import { useSettingsStore } from '@/stores/settings'
import { useUpdateStore } from '@/stores/update'

const ui = useUiStore()
const system = useSystemStore()
const settings = useSettingsStore()
const update = useUpdateStore()

onMounted(() => {
  settings.startAppearanceSync()
  void settings.ensureLoaded()
  system.startPolling()
  update.start()
})
onBeforeUnmount(() => {
  settings.stopAppearanceSync()
  system.stopPolling()
  update.stop()
})
</script>

<template>
  <div class="app-shell">
    <Sidebar :collapsed="ui.sidebarCollapsed" @toggle="ui.toggleSidebar()" />
    <div class="main">
      <PageHeader />
      <UpdateNotice />
      <main class="content">
        <RouterView v-slot="{ Component }">
          <KeepAlive>
            <component :is="Component" />
          </KeepAlive>
        </RouterView>
      </main>
    </div>
  </div>
</template>

<style scoped>
.app-shell { display: flex; height: 100%; min-width: 1080px; min-height: 680px; }
.main { flex: 1; min-width: 0; display: flex; flex-direction: column; }
.content { flex: 1; overflow-y: auto; padding: var(--section-gap) var(--page-pad-x); }
</style>
