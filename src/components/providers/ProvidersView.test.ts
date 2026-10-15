import { beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import catalogJson from '../../../src-tauri/resources/provider_catalog.json'
import statusJson from '../../../src-tauri/resources/provider_status.json'
import oauthJson from '../../../src-tauri/resources/oauth_providers.json'
import { __resetProvidersMock, __setProvidersMockLatency, providersMock } from '@/api/mock/providers'
import { __setOAuthMockTiming } from '@/api/mock/oauth'
import { useProvidersStore } from '@/stores/providers'
import { useUiStore } from '@/stores/ui'
import ProvidersView from '@/views/ProvidersView.vue'

// 页面走真实的 mock API（它读取与 Rust 相同的目录资源），不依赖 Tauri
// 用 Proxy 按方法名转发，这样测试里对 providersMock / oauthMock 的 spyOn 仍然生效
vi.mock('@/api', async () => {
  const { providersMock } = await import('@/api/mock/providers')
  const { oauthMock } = await import('@/api/mock/oauth')
  const parts = [providersMock, oauthMock] as unknown as Record<string, unknown>[]
  const api = new Proxy({} as Record<string, unknown>, {
    get: (_t, key: string) => parts.find((p) => key in p)?.[key],
  })
  return { api }
})
vi.mock('@/api/openExternal', () => ({ openExternal: vi.fn().mockResolvedValue(undefined) }))

const catalog = catalogJson as unknown as { id: string; name: string; category: string; serviceKinds: string[] }[]
const status = statusJson as unknown as Record<string, { adapter: string | null }>
const ready = (id: string) => status[id]?.adapter != null
const oauthFlows = new Map(oauthJson.providers.map((p) => [p.id, p.flow]))
const visible = (c: (typeof catalog)[number]) => ready(c.id) && (c.category !== 'oauth' ||
  (!!oauthFlows.get(c.id) && oauthFlows.get(c.id) !== 'custom' && c.id !== 'github' && c.id !== 'muse'))
const SECRET = 'sk-ui-SUPERSECRET-9876'

async function mountView() {
  const pinia = createPinia()
  setActivePinia(pinia)
  const w = mount(ProvidersView, { global: { plugins: [pinia], stubs: { teleport: true } } })
  await flushPromises()
  return w
}

const apikeyGroup = (w: VueWrapper) => w.find('section[aria-labelledby="g-apikey"]')
const button = (w: VueWrapper, text: string) => w.findAll('button').find((b) => b.text().includes(text))
const cardOf = (w: VueWrapper, name: string) =>
  w.findAll('button.provider-card').find((c) => c.find('.name').text() === name)!

// 注意：VTU 的 teleport 桩会在每次重渲染时重建弹窗内的 DOM（真实浏览器里不会，已用预览验证），
// 所以交互之后一律重新 find，不持有旧的元素引用。
const ADD_FORM = 'form[aria-label="添加 API Key"]'
const addForm = (w: VueWrapper) => w.find(ADD_FORM)
const addKeyEl = (w: VueWrapper) => w.find(`${ADD_FORM} input[aria-label="API Key"]`)
const addLabelEl = (w: VueWrapper) => w.find(`${ADD_FORM} input[aria-label="名称"]`)
const valueOf = (wrapper: { element: Element }) => (wrapper.element as HTMLInputElement).value

async function expandApikey(w: VueWrapper) {
  const btn = apikeyGroup(w).findAll('button').find((b) => b.text().includes('展开'))
  if (btn) await btn.trigger('click')
}

beforeEach(() => {
  __setProvidersMockLatency(0)
  __resetProvidersMock()
  __setOAuthMockTiming({ latencyMs: 0 })
  document.body.innerHTML = ''
})

describe('第四组：API Key 提供商', () => {
  it('只展示适配器就绪的提供商，折叠摘要只统计可用项', async () => {
    const w = await mountView()
    const apikeyCount = catalog.filter((c) => (c.category === 'apikey' || c.category === 'webCookie') && ready(c.id)).length
    expect(apikeyCount).toBeGreaterThan(0)
    expect(apikeyCount).toBeLessThan(87)

    expect(apikeyGroup(w).exists()).toBe(true)
    // 默认全部展开：直接显示全部卡片与 aria-expanded="true"
    expect(apikeyGroup(w).find('[aria-expanded="true"]').exists()).toBe(true)
    expect(apikeyGroup(w).findAll('button.provider-card')).toHaveLength(apikeyCount)
    expect(w.findAll('button.provider-card')).toHaveLength(catalog.filter(visible).length)
    expect(apikeyGroup(w).find('[data-testid="apikey-summary"]').exists()).toBe(false)

    // 组顺序：自定义 → OAuth → 免费 → API Key
    const titles = w.findAll('h2.section-title').map((h) => h.text())
    expect(titles.findIndex((t) => t.includes('OAuth'))).toBeLessThan(titles.findIndex((t) => t.includes('免费')))
    expect(titles.at(-1)).toBe('API Key 提供商')

    // 点击「收起」后折叠
    await button(w, '收起')!.trigger('click')
    expect(apikeyGroup(w).findAll('button.provider-card')).toHaveLength(0)
    expect(apikeyGroup(w).find('[data-testid="apikey-summary"]').exists()).toBe(true)
    expect(apikeyGroup(w).find('[data-testid="apikey-summary"]').text()).toContain(`共 ${apikeyCount} 家`)
    expect(apikeyGroup(w).text()).not.toContain('待接入')

    // 再次点击「展开」后恢复展开
    await expandApikey(w)
    expect(apikeyGroup(w).find('[data-testid="apikey-summary"]').exists()).toBe(false)
    expect(apikeyGroup(w).findAll('button.provider-card')).toHaveLength(apikeyCount)
    expect(apikeyGroup(w).find('[aria-expanded="true"]').exists()).toBe(true)
  })

  it('按能力筛选：点击“语音”只剩 TTS/STT 提供商，再点一次恢复', async () => {
    const w = await mountView()
    await expandApikey(w)
    const expected = catalog.filter(
      (c) => (c.category === 'apikey' || c.category === 'webCookie') && ready(c.id) && c.serviceKinds.some((k) => k === 'tts' || k === 'stt'),
    ).length
    expect(expected).toBeGreaterThan(0)

    const chip = (label: string) => apikeyGroup(w).findAll('.chip').find((c) => c.text().startsWith(label))!
    expect(chip('语音').text()).toContain(String(expected))
    await chip('语音').trigger('click')
    expect(chip('语音').attributes('aria-pressed')).toBe('true')
    const shown = apikeyGroup(w).findAll('button.provider-card')
    expect(shown).toHaveLength(expected)
    expect(shown.map((c) => c.find('.name').text())).toContain('OpenAI')
    expect(shown.map((c) => c.find('.name').text())).not.toContain('ElevenLabs')
    expect(shown.map((c) => c.find('.name').text())).not.toContain('Anthropic')

    await chip('语音').trigger('click')
    expect(apikeyGroup(w).findAll('button.provider-card').length).toBeGreaterThan(expected)
    // 六个能力芯片 + 全部
    expect(apikeyGroup(w).findAll('.chip')).toHaveLength(7)
  })

  it('全局搜索会自动展开该组并只留命中项', async () => {
    const w = await mountView()
    const ui = useUiStore()
    ui.globalQuery = 'perplexity'
    await flushPromises()
    expect(apikeyGroup(w).find('[data-testid="apikey-summary"]').exists()).toBe(false)
    const names = apikeyGroup(w).findAll('button.provider-card .name').map((n) => n.text())
    expect(names.length).toBeGreaterThanOrEqual(2)
    expect(names.every((n) => n.toLowerCase().includes('perplexity'))).toBe(true)

    ui.globalQuery = 'zzz-no-such-provider'
    await flushPromises()
    expect(apikeyGroup(w).text()).toContain('没有匹配')
  })

  it('未就绪的卡片不会出现在列表、搜索结果或筛选计数中', async () => {
    const w = await mountView()
    expect(cardOf(w, 'ElevenLabs')).toBeUndefined()
    expect(cardOf(w, 'Grok Web (Subscription)')).toBeUndefined()
    expect(cardOf(w, 'Cursor IDE')).toBeUndefined()

    const ui = useUiStore()
    ui.globalQuery = 'elevenlabs'
    await flushPromises()
    expect(apikeyGroup(w).findAll('button.provider-card')).toHaveLength(0)
    expect(apikeyGroup(w).find('.chip').text()).toContain('0')

    ui.globalQuery = ''
    await flushPromises()
    const openai = cardOf(w, 'OpenAI')
    expect(openai.findAll('.cap').map((c) => c.text())).toEqual(['聊天', '图像', '语音', '搜索', '嵌入'])
    expect(openai.text()).not.toContain('待接入')
  })

  it('OAuth 授权流程未接入或令牌兑换待接入的提供商不显示，搜索也找不到', async () => {
    const w = await mountView()
    for (const id of ['cline', 'github', 'muse']) {
      const name = catalog.find((p) => p.id === id)!.name
      expect(cardOf(w, name)).toBeUndefined()
      useUiStore().globalQuery = name
      await flushPromises()
      expect(w.find('section[aria-labelledby="g-oauth"]').findAll('button.provider-card')).toHaveLength(0)
    }
  })

  it('OAuth 类：提供“登录验证”入口，并可正常打开授权面板', async () => {
    const w = await mountView()
    await cardOf(w, 'Claude Code').trigger('click')
    await flushPromises()
    // 旧的置灰“授权流程待接入”按钮已被真实入口取代
    expect(w.findAll('button').some((b) => b.text().includes('授权流程待接入'))).toBe(false)
    const oauth = w.findAll('button').find((b) => b.text().includes('登录验证'))!
    expect(oauth.attributes('disabled')).toBeUndefined()
    expect(w.text()).toContain('风险提示')
    // 点击登录验证可打开授权面板
    await oauth.trigger('click')
    await flushPromises()
    expect(w.findComponent({ name: 'OAuthPanel' }).exists()).toBe(true)
  })

  it('专有流程的 OAuth 提供商（Cursor）在目录中隐藏', async () => {
    const w = await mountView()
    expect(cardOf(w, 'Cursor IDE')).toBeUndefined()
    expect(w.text()).not.toContain('Cursor IDE')
  })
})

describe('录入 API Key', () => {
  async function openOpenAi(w: VueWrapper) {
    await expandApikey(w)
    await cardOf(w, 'OpenAI').trigger('click')
    await flushPromises()
  }

  it('免费额度供应商也明确提示填写 API Key', async () => {
    const w = await mountView()
    await cardOf(w, 'OpenRouter').trigger('click')
    await flushPromises()
    expect(button(w, '添加 API Key')).toBeDefined()
    expect(w.text()).toContain('填写从 OpenRouter 官网获取的 API Key')
    await button(w, '添加 API Key')!.trigger('click')
    expect(addForm(w).exists()).toBe(true)
    expect(addForm(w).text()).toContain('备注名称')
    expect(addForm(w).text()).toContain('保存 API Key')
  })

  it('就绪的提供商弹出内嵌表单：密码框，官网链接；提交后立即清空，store 与页面里都没有密钥', async () => {
    const spy = vi.spyOn(providersMock, 'addConnection')
    let release!: () => void
    spy.mockImplementationOnce(
      (input) =>
        new Promise((resolve) => {
          release = () =>
            resolve({ id: 'c-new', providerId: input.providerId, accountLabel: input.accountLabel, kind: 'apiKey', status: 'active', priority: 1, weight: 1, enabled: true })
        }),
    )
    const w = await mountView()
    await openOpenAi(w)

    expect(addForm(w).exists()).toBe(false)
    await button(w, '添加 API Key')!.trigger('click')
    expect(addForm(w).exists()).toBe(true)
    expect(addKeyEl(w).attributes('type')).toBe('password')
    expect(addKeyEl(w).attributes('autocomplete')).toBe('new-password')
    const link = addForm(w).find('a')
    expect(link.attributes('href')).toBe('https://platform.openai.com')
    expect(link.attributes('rel')).toContain('noopener')

    await addLabelEl(w).setValue('个人账号')
    await addKeyEl(w).setValue(SECRET)
    await addForm(w).trigger('submit')

    // 提交后、请求尚未返回时，输入框就已经清空
    expect(valueOf(addKeyEl(w))).toBe('')
    expect(spy).toHaveBeenCalledTimes(1)
    expect(spy).toHaveBeenCalledWith({ providerId: 'openai', accountLabel: '个人账号', apiKey: SECRET })

    release()
    await flushPromises()
    // 成功：表单收起并清空，连接出现在列表里，状态刷新
    expect(addForm(w).exists()).toBe(false)
    expect(w.text()).toContain('个人账号')
    expect(w.text()).toContain('已保存“个人账号”的 API Key')

    const store = useProvidersStore()
    expect(JSON.stringify(store.$state)).not.toContain('SUPERSECRET')
    expect(w.html()).not.toContain('SUPERSECRET')
    expect(document.body.innerHTML).not.toContain('SUPERSECRET')
    expect(store.loaded).toBe(true) // 成功后已重新拉取列表
    spy.mockRestore()
  })

  it('不再提示“有未保存的修改”：连接是立即持久化的，不属于草稿', async () => {
    const w = await mountView()
    await openOpenAi(w)
    await button(w, '添加 API Key')!.trigger('click')
    await addLabelEl(w).setValue('k')
    await addKeyEl(w).setValue(SECRET)
    await addForm(w).trigger('submit')
    await flushPromises()
    expect(w.text()).not.toContain('有未保存的修改')
    const save = w.findAll('button').find((b) => b.text().includes('保存修改'))!
    expect(save.attributes('disabled')).toBeDefined()
  })

  it('本地校验：空标识/空 Key 不发请求；失败时显示错误且密钥框已清空', async () => {
    const spy = vi.spyOn(providersMock, 'addConnection')
    const w = await mountView()
    await openOpenAi(w)
    await button(w, '添加 API Key')!.trigger('click')

    await addForm(w).trigger('submit')
    expect(addForm(w).text()).toContain('请填写名称')
    await addLabelEl(w).setValue('x')
    await addForm(w).trigger('submit')
    expect(addForm(w).text()).toContain('请填写 API Key')
    await addKeyEl(w).setValue('has space')
    await addForm(w).trigger('submit')
    expect(addForm(w).text()).toContain('空白')
    expect(spy).not.toHaveBeenCalled()

    spy.mockRejectedValueOnce(new Error('系统凭据库错误：locked'))
    await addKeyEl(w).setValue(SECRET)
    await addForm(w).trigger('submit')
    await flushPromises()
    expect(valueOf(addKeyEl(w))).toBe('')
    expect(addForm(w).text()).toContain('添加失败：系统凭据库错误：locked')
    expect(addForm(w).text()).not.toContain('SUPERSECRET')
    expect(addForm(w).exists()).toBe(true)
    spy.mockRestore()
  })

  it('取消表单会清空已输入的密钥', async () => {
    const w = await mountView()
    await openOpenAi(w)
    await button(w, '添加 API Key')!.trigger('click')
    await addKeyEl(w).setValue(SECRET)
    await addForm(w).findAll('button').find((b) => b.text() === '取消')!.trigger('click')
    expect(addForm(w).exists()).toBe(false)
    await button(w, '添加 API Key')!.trigger('click')
    expect(valueOf(addKeyEl(w))).toBe('')
  })

  it('已有 API Key 连接可更换密钥：输入框立即清空，store 不含密钥', async () => {
    const spy = vi.spyOn(providersMock, 'updateConnectionCredential')
    const w = await mountView()
    await openOpenAi(w)
    await button(w, '添加 API Key')!.trigger('click')
    await addLabelEl(w).setValue('acct')
    await addKeyEl(w).setValue('sk-first-key')
    await addForm(w).trigger('submit')
    await flushPromises()

    await w.find('button[aria-label="编辑连接 acct"]').trigger('click')
    const newKey = () => w.find('input[aria-label="新的 API Key"]')
    expect(newKey().attributes('type')).toBe('password')
    await newKey().setValue(SECRET)
    await w.find('button[aria-label="更新 API Key"]').trigger('click')
    expect(valueOf(newKey())).toBe('')
    await flushPromises()
    expect(spy).toHaveBeenCalledWith(expect.any(String), SECRET)
    expect(w.text()).toContain('API Key 已更新')
    expect(JSON.stringify(useProvidersStore().$state)).not.toContain('SUPERSECRET')
    spy.mockRestore()
  })
})

describe('自定义提供商', () => {
  const FORM = '#custom-provider-form'
  const field = (w: VueWrapper, label: string) => w.find(`${FORM} [aria-label="${label}"]`)
  async function openCustomDialog(w: VueWrapper, label = '添加 OpenAI 兼容') {
    await button(w, label)!.trigger('click')
    await flushPromises()
  }
  const submit = (w: VueWrapper) => w.find(FORM).trigger('submit')

  it('URL 校验：http 非回环、缺协议、带账号密码都被拦下，不会调用后端', async () => {
    const spy = vi.spyOn(providersMock, 'createCustomProvider')
    const w = await mountView()
    await openCustomDialog(w)
    expect(w.find(FORM).exists()).toBe(true)
    await field(w, '提供商名称').setValue('我的网关')

    for (const [url, msg] of [
      ['http://api.example.com/v1', 'https'],
      ['api.example.com', '格式无效'],
      ['https://user:pw@api.example.com', '用户名或密码'],
      ['', '请填写 Base URL'],
    ] as const) {
      await field(w, 'Base URL').setValue(url)
      await submit(w)
      expect(w.find(`${FORM} .issues`).text(), url).toContain(msg)
    }
    await field(w, '提供商名称').setValue('  ')
    await field(w, 'Base URL').setValue('https://api.example.com/v1')
    await submit(w)
    expect(w.find(`${FORM} .issues`).text()).toContain('请填写提供商名称')
    expect(spy).not.toHaveBeenCalled()
    spy.mockRestore()
  })

  it('协议下拉默认跟随按钮；创建成功后出现在“自定义提供商”组，可含首个 Key（用后即清）', async () => {
    const createSpy = vi.spyOn(providersMock, 'createCustomProvider')
    const addSpy = vi.spyOn(providersMock, 'addConnection')
    const w = await mountView()
    await openCustomDialog(w, '添加 Anthropic 兼容')
    const select = () => field(w, '协议')
    expect(valueOf(select())).toBe('anthropic-messages')
    expect(select().findAll('option').map((o) => o.attributes('value'))).toEqual(['openai-chat', 'anthropic-messages', 'gemini'])

    await select().setValue('gemini')
    await field(w, '提供商名称').setValue('内部 Gemini')
    await field(w, 'Base URL').setValue('http://localhost:8080/v1beta')
    expect(field(w, 'API Key').attributes('type')).toBe('password')
    await field(w, 'API Key').setValue(SECRET)
    await submit(w)
    expect(valueOf(field(w, 'API Key'))).toBe('')
    await flushPromises()

    expect(createSpy).toHaveBeenCalledWith({ name: '内部 Gemini', protocol: 'gemini', baseUrl: 'http://localhost:8080/v1beta' })
    expect(addSpy).toHaveBeenCalledTimes(1)
    expect(addSpy.mock.calls[0][0]).toMatchObject({ accountLabel: '默认账户', apiKey: SECRET })
    expect(w.find(FORM).exists()).toBe(false)
    expect(w.findAll('button.provider-card').some((c) => c.find('.name').text() === '内部 Gemini')).toBe(true)
    const store = useProvidersStore()
    const created = store.byGroup('custom')
    expect(created).toHaveLength(1)
    expect(created[0].connectionCount).toBe(1)
    expect(JSON.stringify(store.$state)).not.toContain('SUPERSECRET')
    createSpy.mockRestore()
    addSpy.mockRestore()
  })

  it('首个 Key 保存失败时提供商已创建：提示原因，重试只补 Key，不会重复创建', async () => {
    const createSpy = vi.spyOn(providersMock, 'createCustomProvider')
    const addSpy = vi.spyOn(providersMock, 'addConnection')
    addSpy.mockRejectedValueOnce(new Error('凭据库不可用'))
    const w = await mountView()
    await openCustomDialog(w)
    await field(w, '提供商名称').setValue('半成品')
    await field(w, 'Base URL').setValue('https://gw.example.com')
    await field(w, 'API Key').setValue(SECRET)
    await submit(w)
    await flushPromises()
    expect(w.find(`${FORM} .issues`).text()).toContain('提供商已创建，但保存 API Key 失败：凭据库不可用')
    expect(valueOf(field(w, 'API Key'))).toBe('')
    expect(field(w, 'Base URL').attributes('disabled')).toBeDefined()

    await field(w, 'API Key').setValue(SECRET)
    await submit(w)
    await flushPromises()
    expect(createSpy).toHaveBeenCalledTimes(1)
    expect(addSpy).toHaveBeenCalledTimes(2)
    expect(w.find(FORM).exists()).toBe(false)
    expect(useProvidersStore().byGroup('custom')).toHaveLength(1)
    createSpy.mockRestore()
    addSpy.mockRestore()
  })

  it('删除自定义提供商要经过确认，确认后从列表消失', async () => {
    const w = await mountView()
    await openCustomDialog(w)
    await field(w, '提供商名称').setValue('临时网关')
    await field(w, 'Base URL').setValue('https://gw.example.com/v1')
    await submit(w)
    await flushPromises()

    await cardOf(w, '临时网关').trigger('click')
    await flushPromises()
    const del = w.find('button[aria-label="删除提供商"]')
    expect(del.exists()).toBe(true)
    await del.trigger('click')
    // 确认前仍在
    expect(useProvidersStore().byGroup('custom')).toHaveLength(1)
    const confirm = w.findAll('button').find((b) => b.text() === '删除')!
    await confirm.trigger('click')
    await flushPromises()
    expect(useProvidersStore().byGroup('custom')).toHaveLength(0)
    expect(w.findAll('button.provider-card').some((c) => c.find('.name').text() === '临时网关')).toBe(false)
  })

  it('内置提供商的弹窗没有“删除提供商”', async () => {
    const w = await mountView()
    await cardOf(w, 'OpenRouter').trigger('click')
    await flushPromises()
    expect(w.find('button[aria-label="删除提供商"]').exists()).toBe(false)
  })
})

describe('测试全部（testProviders）', () => {
  it('免费组：有连接的就绪提供商经 testProviders 如实得到失败结果，未接入的被跳过', async () => {
    const spy = vi.spyOn(providersMock, 'testProviders')
    const w = await mountView()
    await w.find('button[aria-label="测试全部免费额度提供商"]').trigger('click')
    await flushPromises()
    expect(spy).toHaveBeenCalledTimes(1)
    expect(spy.mock.calls[0][0]).toEqual(['openrouter']) // 免费组里仅它既就绪又有（演示）连接
    const card = cardOf(w, 'OpenRouter')
    expect(card.text()).toContain('测试失败 1/1')
    expect(card.text()).not.toContain('测试通过')
    spy.mockRestore()
  })

  it('API Key 组没有任何连接时给出说明而不是静默', async () => {
    const w = await mountView()
    await apikeyGroup(w).find('button[aria-label="测试全部 API Key 提供商"]').trigger('click')
    await flushPromises()
    expect(apikeyGroup(w).find('.note').text()).toContain('没有已添加连接')
  })
})
