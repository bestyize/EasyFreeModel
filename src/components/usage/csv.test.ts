import { describe, expect, it } from 'vitest'
import type { UsageRequest } from '@/types'
import { CSV_COLUMNS, escapeCsvCell, requestsToCsv } from './csv'

const row = (over: Partial<UsageRequest> = {}): UsageRequest => ({
  requestId: 'req_1',
  providerId: 'openrouter',
  accountId: 'acct-1',
  model: 'gpt-5-mini',
  inputTokens: 100,
  outputTokens: 20,
  cachedTokens: 5,
  latencyMs: 321,
  status: 'success',
  estimatedCost: 0.0012,
  tokenSource: 'upstream',
  timestamp: '2026-10-14T01:02:03.000Z',
  ...over,
})

describe('escapeCsvCell', () => {
  it.each(['=1+1', '+SUM(A1)', '-2+3', '@cmd', '\tcmd', '\rcmd'])('以危险字符开头的 %j 前加单引号', (v) => {
    expect(escapeCsvCell(v).startsWith("'") || escapeCsvCell(v).startsWith('"\'')).toBe(true)
  })

  it('公式字符串被中和：=HYPERLINK(...) -> \'=HYPERLINK(...)', () => {
    expect(escapeCsvCell('=HYPERLINK("http://x")')).toBe(`"'=HYPERLINK(""http://x"")"`)
  })

  it('普通字符串不变；中间出现的 = 不处理', () => {
    expect(escapeCsvCell('gpt-5')).toBe('gpt-5')
    expect(escapeCsvCell('a=b')).toBe('a=b')
  })

  it('含逗号/引号/换行时加引号并转义', () => {
    expect(escapeCsvCell('a,b')).toBe('"a,b"')
    expect(escapeCsvCell('say "hi"')).toBe('"say ""hi"""')
    expect(escapeCsvCell('l1\nl2')).toBe('"l1\nl2"')
  })

  it('数字原样输出（负数不是公式），null/undefined 为空', () => {
    expect(escapeCsvCell(-5)).toBe('-5')
    expect(escapeCsvCell(0)).toBe('0')
    expect(escapeCsvCell(null)).toBe('')
    expect(escapeCsvCell(undefined)).toBe('')
  })
})

describe('requestsToCsv', () => {
  it('表头包含规范要求的全部字段', () => {
    const header = requestsToCsv([]).replace('\uFEFF', '').split('\r\n')[0]!.split(',')
    for (const f of [
      'request_id', 'provider_id', 'account_id', 'model', 'input_tokens', 'output_tokens',
      'cached_tokens', 'latency_ms', 'status', 'estimated_cost', 'timestamp',
    ]) {
      expect(header).toContain(f)
    }
    expect(header).toEqual([...CSV_COLUMNS])
  })

  it('以 BOM 开头，行以 CRLF 结束，缓存 Token 单独成列', () => {
    const csv = requestsToCsv([row()])
    expect(csv.startsWith('\uFEFF')).toBe(true)
    expect(csv.endsWith('\r\n')).toBe(true)
    const cells = csv.replace('\uFEFF', '').split('\r\n')[1]!.split(',')
    expect(cells[CSV_COLUMNS.indexOf('input_tokens')]).toBe('100')
    expect(cells[CSV_COLUMNS.indexOf('cached_tokens')]).toBe('5')
  })

  it('用户可控字段（model/account）中的公式被中和', () => {
    const csv = requestsToCsv([row({ model: '=cmd|calc', accountId: '@evil' })])
    const line = csv.split('\r\n')[1]!
    expect(line).toContain("'=cmd|calc")
    expect(line).toContain("'@evil")
    expect(line).not.toMatch(/(^|,)=cmd/)
  })
})
