import { describe, expect, it } from 'vitest'
import { fieldState, MEDIA_CAPABILITIES, PROVIDER_MEDIA_FEATURES } from './capabilities'
import { buildParams, defaultValues, validateFields } from './forms'
import { parseSearchPayload } from './searchResults'

const imageFields = MEDIA_CAPABILITIES.image.modes[0].fields
const editFields = MEDIA_CAPABILITIES.image.modes[1].fields
const asrFields = MEDIA_CAPABILITIES.asr.modes[0].fields

function file(name: string, size = 10, type = 'application/octet-stream') {
  const f = new File(['x'], name, { type })
  Object.defineProperty(f, 'size', { value: size })
  return f
}

describe('能力表驱动的表单', () => {
  it('文生图不含图片输入，图片编辑含必填图片字段', () => {
    expect(imageFields.some((f) => f.key === 'image')).toBe(false)
    expect(editFields.find((f) => f.key === 'image')?.required).toBe(true)
  })

  it('每种能力都有说明“适配器尚未接入”的无供应商文案', () => {
    for (const cap of Object.values(MEDIA_CAPABILITIES)) expect(cap.unavailableReason).toContain('适配器尚未接入')
  })

  it('ASR 时间戳 / 说话人分离在供应商未声明支持时被禁用且不进入参数', async () => {
    const ts = asrFields.find((f) => f.key === 'timestamps')!
    expect(fieldState(ts, 'any-provider')).toBe('disabled')
    const values = { ...defaultValues(asrFields), audio: file('a.mp3'), timestamps: true }
    const params = await buildParams(asrFields, values, 'any-provider')
    expect(params).not.toHaveProperty('timestamps')
    expect(params).not.toHaveProperty('diarization')
  })

  it('供应商声明支持后字段启用', () => {
    PROVIDER_MEDIA_FEATURES['p-test'] = ['asr-timestamps']
    try {
      const ts = asrFields.find((f) => f.key === 'timestamps')!
      expect(fieldState(ts, 'p-test')).toBe('enabled')
    } finally {
      delete PROVIDER_MEDIA_FEATURES['p-test']
    }
  })
})

describe('validateFields', () => {
  it('必填提示词', () => {
    expect(validateFields(imageFields, defaultValues(imageFields)).prompt).toBeTruthy()
  })
  it('张数为 1–4 的整数，种子为非负整数', () => {
    const base = { ...defaultValues(imageFields), prompt: 'cat' }
    expect(validateFields(imageFields, { ...base, n: 5 }).n).toBeTruthy()
    expect(validateFields(imageFields, { ...base, n: 1.5 }).n).toBeTruthy()
    expect(validateFields(imageFields, { ...base, seed: -1 }).seed).toBeTruthy()
    expect(validateFields(imageFields, { ...base, n: 2, seed: 42 })).toEqual({})
  })
  it('音频格式与大小限制', () => {
    const base = defaultValues(asrFields)
    expect(validateFields(asrFields, { ...base, audio: file('x.exe', 10) }).audio).toContain('不支持的文件格式')
    expect(validateFields(asrFields, { ...base, audio: file('x.mp3', 26 * 1024 * 1024) }).audio).toContain('文件过大')
    expect(validateFields(asrFields, { ...base, audio: file('x.MP3', 1024) }).audio).toBeUndefined()
    expect(validateFields(asrFields, base).audio).toBeTruthy()
  })
  it('TTS 文本超过最大字数', () => {
    const tts = MEDIA_CAPABILITIES.tts.modes[0].fields
    const v = { ...defaultValues(tts), voice: 'v', text: 'a'.repeat(4097) }
    expect(validateFields(tts, v).text).toContain('4096')
  })
})

describe('buildParams', () => {
  it('只包含非空字段，数字转 number，文件转载荷', async () => {
    const v = { ...defaultValues(editFields), prompt: '  改成蓝色  ', n: 2, seed: '', image: file('a.png', 3, 'image/png') }
    const p = await buildParams(editFields, v)
    expect(p.prompt).toBe('改成蓝色')
    expect(p.n).toBe(2)
    expect(p).not.toHaveProperty('seed')
    expect(p.image).toMatchObject({ name: 'a.png', mimeType: 'image/png' })
    expect(typeof (p.image as { dataBase64: string }).dataBase64).toBe('string')
  })
})

describe('parseSearchPayload', () => {
  it('解析来源类型并丢弃非 http(s) 链接', () => {
    const p = parseSearchPayload(
      JSON.stringify({
        origin: 'aggregated',
        results: [
          { title: 'A', url: 'https://a.com/x', snippet: 's' },
          { title: 'evil', url: 'javascript:alert(1)' },
          { url: 'http://b.com' },
        ],
      }),
    )
    expect(p?.origin).toBe('aggregated')
    expect(p?.results.map((r) => r.url)).toEqual(['https://a.com/x', 'http://b.com/'])
  })
  it('非 JSON 返回 null，不编造结果', () => {
    expect(parseSearchPayload('hello')).toBeNull()
    expect(parseSearchPayload('{"foo":1}')).toBeNull()
  })
})
