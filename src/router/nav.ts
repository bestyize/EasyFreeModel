import {
  ChartColumn,
  ChartPie,
  Database,
  Globe,
  Image,
  KeyRound,
  Mic,
  Settings,
  Video,
  Volume2,
} from 'lucide-vue-next'
import type { Component } from 'vue'

export const APP_NAME = 'Router Desktop'
export const APP_VERSION = '0.5.99'

export interface NavItem {
  path: string
  name: string
  label: string
  subtitle: string
  icon: Component
}

/** 固定十项一级菜单，顺序与 PRODUCT_DESIGN.md §2 一致。不得新增一级导航。 */
export const NAV_ITEMS: NavItem[] = [
  { path: '/api-keys', name: 'api-keys', label: 'API 密钥', subtitle: '管理对外提供的本地访问密钥、权限与有效期', icon: KeyRound },
  { path: '/providers', name: 'providers', label: '提供商', subtitle: '管理您的 AI 提供商、账户与模型', icon: Database },
  { path: '/usage', name: 'usage', label: '使用情况', subtitle: '监控您的 API 请求、Token 消耗和调用趋势', icon: ChartColumn },
  { path: '/quota', name: 'quota', label: '配额跟踪', subtitle: '跟踪和管理您的 API 配额限制', icon: ChartPie },
  { path: '/image', name: 'image', label: '图像生成', subtitle: '文生图与图片编辑，查看任务历史', icon: Image },
  { path: '/asr', name: 'asr', label: '语音转文字', subtitle: '上传或录制音频并转写为文本', icon: Mic },
  { path: '/tts', name: 'tts', label: '文字转语音', subtitle: '合成语音、试听并导出音频', icon: Volume2 },
  { path: '/video', name: 'video', label: '视频生成', subtitle: '创建并查看长耗时视频生成任务', icon: Video },
  { path: '/websearch', name: 'websearch', label: 'WebSearch', subtitle: '联网搜索供应商接入与结果测试', icon: Globe },
  { path: '/settings', name: 'settings', label: '设置页', subtitle: '本地代理、网络、安全、更新与数据备份', icon: Settings },
]
