import {
  ChartColumn,
  ChartPie,
  Database,
  Globe,
  Image,
  KeyRound,
  Link2,
  Mic,
  Network,
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
  /** 暂时从左侧菜单隐藏，路由仍保留 */
  hidden?: boolean
}

/** 一级菜单；媒体类入口暂时隐藏，代理池按用户要求置于设置页上方。 */
export const NAV_ITEMS: NavItem[] = [
  { path: '/api-keys', name: 'api-keys', label: 'API 密钥', subtitle: '管理本地访问密钥，并一键接入 Codex / Claude Code / CC Switch', icon: KeyRound },
  { path: '/api-guide', name: 'api-guide', label: 'API 接入', subtitle: '网关地址与各协议的请求示例', icon: Link2 },
  { path: '/providers', name: 'providers', label: '提供商', subtitle: '管理您的 AI 提供商、账户与模型', icon: Database },
  { path: '/usage', name: 'usage', label: '使用情况', subtitle: '监控您的 API 请求、Token 消耗和调用趋势', icon: ChartColumn },
  { path: '/quota', name: 'quota', label: '配额跟踪', subtitle: '跟踪和管理您的 API 配额限制', icon: ChartPie },
  { path: '/image', name: 'image', label: '图像生成', subtitle: '文生图与图片编辑，查看任务历史', icon: Image, hidden: true },
  { path: '/asr', name: 'asr', label: '语音转文字', subtitle: '上传或录制音频并转写为文本', icon: Mic, hidden: true },
  { path: '/tts', name: 'tts', label: '文字转语音', subtitle: '合成语音、试听并导出音频', icon: Volume2, hidden: true },
  { path: '/video', name: 'video', label: '视频生成', subtitle: '创建并查看长耗时视频生成任务', icon: Video, hidden: true },
  { path: '/websearch', name: 'websearch', label: 'WebSearch', subtitle: '联网搜索供应商接入与结果测试', icon: Globe, hidden: true },
  { path: '/proxy-pool', name: 'proxy-pool', label: '代理池', subtitle: '管理出站代理节点与轮换策略', icon: Network },
  { path: '/settings', name: 'settings', label: '设置页', subtitle: '本地代理、网络、安全、更新与数据备份', icon: Settings },
]

export const VISIBLE_NAV_ITEMS: NavItem[] = NAV_ITEMS.filter((item) => !item.hidden)
