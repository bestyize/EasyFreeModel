import { createRouter, createWebHashHistory } from 'vue-router'
import { NAV_ITEMS } from './nav'

const views = {
  'api-keys': () => import('@/views/ApiKeysView.vue'),
  providers: () => import('@/views/ProvidersView.vue'),
  usage: () => import('@/views/UsageView.vue'),
  quota: () => import('@/views/QuotaView.vue'),
  image: () => import('@/views/ImageView.vue'),
  asr: () => import('@/views/AsrView.vue'),
  tts: () => import('@/views/TtsView.vue'),
  video: () => import('@/views/VideoView.vue'),
  websearch: () => import('@/views/WebSearchView.vue'),
  settings: () => import('@/views/SettingsView.vue'),
} as const

export const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: '/', redirect: '/providers' },
    ...NAV_ITEMS.map((item) => ({
      path: item.path,
      name: item.name,
      component: views[item.name as keyof typeof views],
      meta: { title: item.label, subtitle: item.subtitle },
    })),
  ],
})
