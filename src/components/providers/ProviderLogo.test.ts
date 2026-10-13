import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import ProviderLogo from './ProviderLogo.vue'
import { capabilityLabels, logoText, readableOn } from './status'

describe('ProviderLogo', () => {
  it('uses the catalog colour and text icon as a flat colour block (no gradient)', () => {
    const w = mount(ProviderLogo, { props: { name: 'Alibaba Coding', color: '#FF6A00', textIcon: 'ALi' } })
    expect(w.text()).toBe('ALi')
    const style = w.attributes('style') ?? ''
    expect(style).toContain('rgb(255, 106, 0)')
    expect(style).not.toContain('gradient')
  })

  it('falls back to the first letter and a hashed colour when the catalog has none', () => {
    const w = mount(ProviderLogo, { props: { name: 'kimi' } })
    expect(w.text()).toBe('K')
  })

  it('switches to dark text with a hairline border on near-white colours', () => {
    const w = mount(ProviderLogo, { props: { name: 'Chutes AI', color: '#FFFFFF', textIcon: 'CH' } })
    expect(w.attributes('style')).toContain('rgb(22, 24, 29)')
    expect(w.attributes('style')).toContain('inset')
    expect(readableOn('#FFFFFF')).toBe('#16181d')
    expect(readableOn('#FF2442')).toBe('#ffffff')
    expect(readableOn('not-a-colour')).toBe('#ffffff')
  })

  it('caps the text icon at three characters', () => {
    expect(logoText('X', 'ABCDE')).toBe('ABC')
    expect(logoText('Zed', null)).toBe('Z')
    expect(logoText('Zed', '  ')).toBe('Z')
  })
})

describe('capabilityLabels', () => {
  it('folds raw service kinds into the six UI capabilities, in fixed order, deduplicated', () => {
    expect(capabilityLabels(['llm', 'embedding', 'tts', 'stt', 'image', 'imageToText', 'webSearch'])).toEqual([
      '聊天',
      '图像',
      '语音',
      '搜索',
      '嵌入',
    ])
    expect(capabilityLabels(['systemone'])).toEqual([])
    expect(capabilityLabels([])).toEqual([])
  })
})
