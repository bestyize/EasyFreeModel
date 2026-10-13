import type { MediaKind } from '@/types'

/**
 * 多模态“能力表”：表单字段完全由这里的数据驱动，页面只渲染对应能力支持的字段，
 * 避免向不支持的上游发送字段（PRODUCT_DESIGN §4.5）。
 *
 * 注意：契约（MediaApi）目前没有“按供应商查询媒体能力”的接口，因此：
 *  - 基础字段（提示词、尺寸等）按通用能力列出；
 *  - 只有部分供应商支持的高级字段用 `requires` 标记特性名，
 *    由 PROVIDER_MEDIA_FEATURES 声明哪些供应商具备；未声明则禁用，不假设支持。
 */

export type FieldType = 'text' | 'textarea' | 'select' | 'number' | 'switch' | 'file'
export type FieldValue = string | number | boolean | File | null

export interface FieldOption {
  value: string | number
  label: string
}

export interface FieldDef {
  key: string
  label: string
  type: FieldType
  required?: boolean
  placeholder?: string
  hint?: string
  options?: FieldOption[]
  min?: number
  max?: number
  step?: number
  integer?: boolean
  maxLength?: number
  default?: string | number | boolean
  /** file：允许的扩展名（小写，不含点） */
  accept?: string[]
  /** file：最大字节数 */
  maxBytes?: number
  /** 需要供应商声明具备的特性；未声明时按 whenUnsupported 处理 */
  requires?: string
  whenUnsupported?: 'disable' | 'hide'
}

export interface ModeDef {
  id: string
  label: string
  fields: FieldDef[]
}

export interface MediaCapability {
  kind: MediaKind
  title: string
  /** 无可用供应商时 EmptyState 的说明 */
  unavailableReason: string
  modes: ModeDef[]
}

const MB = 1024 * 1024

/** 供应商 id -> 已确认支持的媒体特性。后端适配器接入后在此登记；当前为空，意味着没有任何高级特性被假设支持。 */
export const PROVIDER_MEDIA_FEATURES: Record<string, readonly string[]> = {}

export function providerSupports(providerId: string | null | undefined, feature: string): boolean {
  return !!providerId && (PROVIDER_MEDIA_FEATURES[providerId]?.includes(feature) ?? false)
}

export type FieldState = 'enabled' | 'disabled' | 'hidden'

export function fieldState(field: FieldDef, providerId: string | null | undefined): FieldState {
  if (!field.requires || providerSupports(providerId, field.requires)) return 'enabled'
  return field.whenUnsupported === 'hide' ? 'hidden' : 'disabled'
}

export const IMAGE_FILE = { accept: ['png', 'jpg', 'jpeg', 'webp'], maxBytes: 10 * MB }
export const AUDIO_FILE = {
  accept: ['mp3', 'wav', 'm4a', 'flac', 'ogg', 'webm', 'mp4', 'mpeg', 'mpga'],
  maxBytes: 25 * MB,
  /** 录音时长上限（秒） */
  maxRecordSeconds: 600,
}
export const TTS_MAX_CHARS = 4096
export const TTS_FORMATS = ['mp3', 'wav', 'opus', 'flac']

const sizeField: FieldDef = {
  key: 'size',
  label: '尺寸',
  type: 'select',
  default: '1024x1024',
  options: [
    { value: '512x512', label: '512 × 512' },
    { value: '768x768', label: '768 × 768' },
    { value: '1024x1024', label: '1024 × 1024' },
    { value: '1024x1536', label: '1024 × 1536（竖）' },
    { value: '1536x1024', label: '1536 × 1024（横）' },
  ],
}
const countField: FieldDef = { key: 'n', label: '张数', type: 'number', integer: true, min: 1, max: 4, default: 1 }
const seedField: FieldDef = {
  key: 'seed',
  label: '种子',
  type: 'number',
  integer: true,
  min: 0,
  max: 4294967295,
  placeholder: '留空为随机',
  hint: '相同种子与参数可复现结果，留空则随机。',
}
const imagePrompt: FieldDef = { key: 'prompt', label: '提示词', type: 'textarea', required: true, maxLength: 4000, placeholder: '描述想要生成的画面' }

const videoCommon: FieldDef[] = [
  {
    key: 'resolution',
    label: '分辨率',
    type: 'select',
    default: '720p',
    options: [
      { value: '480p', label: '480p' },
      { value: '720p', label: '720p' },
      { value: '1080p', label: '1080p' },
    ],
  },
  {
    key: 'aspectRatio',
    label: '画面比例',
    type: 'select',
    default: '16:9',
    options: [
      { value: '16:9', label: '16:9' },
      { value: '9:16', label: '9:16' },
      { value: '1:1', label: '1:1' },
      { value: '4:3', label: '4:3' },
    ],
  },
  {
    key: 'duration',
    label: '时长（秒）',
    type: 'select',
    default: 5,
    options: [3, 5, 8, 10, 15].map((s) => ({ value: s, label: `${s} 秒` })),
  },
  {
    key: 'fps',
    label: '帧率',
    type: 'select',
    default: 24,
    options: [24, 30].map((f) => ({ value: f, label: `${f} fps` })),
  },
]

export const MEDIA_CAPABILITIES: Record<MediaKind, MediaCapability> = {
  image: {
    kind: 'image',
    title: '图像生成',
    unavailableReason: '后端图像适配器尚未接入，暂无已就绪的供应商可处理图像请求。',
    modes: [
      { id: 'generate', label: '文生图', fields: [imagePrompt, sizeField, countField, seedField] },
      {
        id: 'edit',
        label: '图片编辑',
        fields: [
          { key: 'image', label: '待编辑图片', type: 'file', required: true, accept: IMAGE_FILE.accept, maxBytes: IMAGE_FILE.maxBytes, hint: 'PNG / JPG / WebP，最大 10 MB。' },
          { ...imagePrompt, placeholder: '描述希望如何修改这张图片' },
          sizeField,
          countField,
          seedField,
        ],
      },
    ],
  },
  asr: {
    kind: 'asr',
    title: '语音转文字',
    unavailableReason: '后端语音识别适配器尚未接入，暂无已就绪的供应商可处理转写请求。',
    modes: [
      {
        id: 'transcribe',
        label: '转写',
        fields: [
          { key: 'audio', label: '音频文件', type: 'file', required: true, accept: AUDIO_FILE.accept, maxBytes: AUDIO_FILE.maxBytes },
          {
            key: 'language',
            label: '语言',
            type: 'select',
            default: '',
            options: [
              { value: '', label: '自动检测' },
              { value: 'zh', label: '中文' },
              { value: 'en', label: '英语' },
              { value: 'ja', label: '日语' },
              { value: 'ko', label: '韩语' },
              { value: 'es', label: '西班牙语' },
              { value: 'fr', label: '法语' },
              { value: 'de', label: '德语' },
            ],
          },
          { key: 'timestamps', label: '输出时间戳', type: 'switch', default: false, requires: 'asr-timestamps', whenUnsupported: 'disable' },
          { key: 'diarization', label: '说话人分离', type: 'switch', default: false, requires: 'asr-diarization', whenUnsupported: 'disable' },
        ],
      },
    ],
  },
  tts: {
    kind: 'tts',
    title: '文字转语音',
    unavailableReason: '后端语音合成适配器尚未接入，暂无已就绪的云端供应商可处理合成请求（本地 TTS 为后期扩展）。',
    modes: [
      {
        id: 'synthesize',
        label: '合成',
        fields: [
          { key: 'text', label: '文本', type: 'textarea', required: true, maxLength: TTS_MAX_CHARS, placeholder: '输入要合成的文字' },
          { key: 'voice', label: '音色', type: 'text', required: true, placeholder: '填写供应商支持的音色 ID', hint: '可用音色取决于供应商与模型。' },
          { key: 'speed', label: '语速', type: 'number', min: 0.5, max: 2, step: 0.1, default: 1, hint: '0.5–2，1 为正常语速。' },
          { key: 'pitch', label: '音调（半音）', type: 'number', min: -12, max: 12, step: 0.5, default: 0, requires: 'tts-pitch', whenUnsupported: 'disable', hint: '-12–12，0 为原调。' },
          { key: 'format', label: '音频格式', type: 'select', default: 'mp3', options: TTS_FORMATS.map((f) => ({ value: f, label: f.toUpperCase() })) },
        ],
      },
    ],
  },
  video: {
    kind: 'video',
    title: '视频生成',
    unavailableReason: '后端视频适配器尚未接入，暂无已就绪的供应商可处理视频任务。',
    modes: [
      {
        id: 'text2video',
        label: '文生视频',
        fields: [{ key: 'prompt', label: '提示词', type: 'textarea', required: true, maxLength: 2000, placeholder: '描述视频内容与镜头' }, ...videoCommon],
      },
      {
        id: 'image2video',
        label: '图生视频',
        fields: [
          { key: 'image', label: '首帧图片', type: 'file', required: true, accept: IMAGE_FILE.accept, maxBytes: IMAGE_FILE.maxBytes, hint: 'PNG / JPG / WebP，最大 10 MB。' },
          { key: 'prompt', label: '提示词（可选）', type: 'textarea', maxLength: 2000, placeholder: '描述画面如何运动' },
          ...videoCommon,
        ],
      },
    ],
  },
  search: {
    kind: 'search',
    title: 'WebSearch',
    unavailableReason: '后端搜索适配器尚未接入，暂无已就绪的供应商可处理搜索请求。',
    modes: [
      {
        id: 'search',
        label: '搜索',
        fields: [
          { key: 'query', label: '查询', type: 'text', required: true, maxLength: 500, placeholder: '输入搜索关键词' },
          {
            key: 'resultMode',
            label: '结果来源',
            type: 'select',
            default: 'native',
            hint: '原生：由供应商搜索接口直接返回；聚合：由本应用合并多个来源，并非上游原生结果。',
            options: [
              { value: 'native', label: '原生搜索接口' },
              { value: 'aggregated', label: '聚合结果' },
            ],
          },
          {
            key: 'region',
            label: '地域',
            type: 'select',
            default: '',
            options: [
              { value: '', label: '不限' },
              { value: 'CN', label: '中国' },
              { value: 'US', label: '美国' },
              { value: 'JP', label: '日本' },
              { value: 'GB', label: '英国' },
            ],
          },
          {
            key: 'timeRange',
            label: '时间范围',
            type: 'select',
            default: '',
            options: [
              { value: '', label: '不限' },
              { value: 'day', label: '过去 24 小时' },
              { value: 'week', label: '过去一周' },
              { value: 'month', label: '过去一月' },
              { value: 'year', label: '过去一年' },
            ],
          },
          { key: 'count', label: '结果数量', type: 'number', integer: true, min: 1, max: 20, default: 5 },
        ],
      },
    ],
  },
}
