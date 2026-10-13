import { describe, expect, it } from 'vitest'
import { validateAccountLabel, validateApiKey, validateBaseUrl, validateCustomProviderForm } from './validation'

describe('validateBaseUrl（与 Rust validate_base_url 同一套规则）', () => {
  it.each([
    ['https://api.example.com/v1', 'https://api.example.com/v1'],
    ['  https://api.example.com/v1/  ', 'https://api.example.com/v1'],
    ['https://api.example.com', 'https://api.example.com'],
    ['http://localhost:11434', 'http://localhost:11434'],
    ['http://127.0.0.1:8080/v1', 'http://127.0.0.1:8080/v1'],
    ['http://127.5.5.5/v1', 'http://127.5.5.5/v1'],
    ['http://[::1]:9000', 'http://[::1]:9000'],
  ])('accepts %s', (input, normalized) => {
    expect(validateBaseUrl(input)).toEqual({ ok: true, url: normalized })
  })

  it.each([
    '',
    '   ',
    'api.example.com',
    'http://api.example.com/v1',
    'http://192.168.1.2/v1',
    'ftp://example.com',
    'https://',
    'https://user:pw@example.com',
    'https://example.com/v1?key=abc',
    'https://example.com/#frag',
    'javascript:alert(1)',
    'file:///etc/passwd',
  ])('rejects %j', (input) => {
    const r = validateBaseUrl(input)
    expect(r.ok).toBe(false)
    if (!r.ok) expect(r.message.length).toBeGreaterThan(0)
  })

  it('http is only allowed on loopback, with an explicit message', () => {
    const r = validateBaseUrl('http://example.com')
    expect(r).toMatchObject({ ok: false })
    expect((r as { message: string }).message).toContain('https')
  })
})

describe('validateCustomProviderForm', () => {
  const ok = { name: 'Mine', protocol: 'openai-chat' as const, baseUrl: 'https://a.example.com/v1' }
  it('passes a valid form', () => expect(validateCustomProviderForm(ok)).toEqual([]))
  it('collects every problem', () => {
    const issues = validateCustomProviderForm({ ...ok, name: '  ', baseUrl: 'http://a.example.com' })
    expect(issues).toHaveLength(2)
    expect(issues[0]).toContain('名称')
    expect(issues[1]).toContain('https')
  })
  it('limits the name length', () => {
    expect(validateCustomProviderForm({ ...ok, name: 'x'.repeat(65) })).toHaveLength(1)
  })
})

describe('validateApiKey / validateAccountLabel', () => {
  it('rejects empty, spaced and overlong keys without echoing them', () => {
    for (const k of ['', '   ', 'has space', 'a'.repeat(4097)]) {
      const msg = validateApiKey(k)
      expect(msg).toBeTruthy()
      if (k.trim()) expect(msg).not.toContain(k.trim())
    }
    expect(validateApiKey('  sk-abc  ')).toBeNull()
  })
  it('validates the account label', () => {
    expect(validateAccountLabel('')).toBeTruthy()
    expect(validateAccountLabel('x'.repeat(129))).toBeTruthy()
    expect(validateAccountLabel('个人账号')).toBeNull()
  })
})
