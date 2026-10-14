<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { AlertTriangle, ExternalLink } from 'lucide-vue-next'
import { openExternal } from '@/api/openExternal'
import { oauthErrorText, toOAuthApiError } from '@/api/oauthError'
import { useOAuthStore } from '@/stores/oauth'
import type { Connection, OAuthStartResult } from '@/types'
import DeviceCodeCard from './DeviceCodeCard.vue'
import OAuthCredentialForm from './OAuthCredentialForm.vue'

/**
 * 单个提供商的 OAuth 授权面板（内嵌使用）。
 * 授权在系统浏览器里完成，不内嵌 WebView。前端只持有：授权地址、设备码页面的 user_code 与
 * verification_uri、状态枚举、脱敏错误码——令牌与授权码只存在于 Rust 侧。
 */
const props = defineProps<{
  providerId: string
  providerName: string
  /** 重新授权：传入需要重新授权的 OAuth 连接 */
  reauthConnection?: Connection | null
  /** 外层已经展示了风险提示时，面板内不再重复 */
  hideRiskNotice?: boolean
}>()
const emit = defineEmits<{
  /** 授权成功（连接已创建/恢复） */
  done: [connectionId: string]
  /** 用户关闭面板 */
  close: []
  /** 用户取消了进行中的授权（已调用 oauthCancel） */
  cancel: []
}>()

const POLL_INTERVAL_MS = 1500

const store = useOAuthStore()

type Stage = 'form' | 'starting' | 'running' | 'succeeded' | 'failed' | 'cancelled' | 'expired'

const initializing = ref(true)
const stage = ref<Stage>('form')
const forceCredentials = ref(false)
const label = ref('')
const session = ref<OAuthStartResult | null>(null)
const expiresAtMs = ref(0)
const now = ref(Date.now())
const errorText = ref<string | null>(null)
const openError = ref<string | null>(null)
const resultConnectionId = ref<string | null>(null)

let pollTimer: ReturnType<typeof setTimeout> | undefined
let tickTimer: ReturnType<typeof setTimeout> | undefined
let disposed = false

const info = computed(() => store.infoFor(props.providerId))
const isReauth = computed(() => !!props.reauthConnection)

type View =
  | 'loading'
  | 'loadError'
  | 'custom'
  | 'credentials'
  | 'idle'
  | 'starting'
  | 'running'
  | 'succeeded'
  | 'failed'
  | 'cancelled'
  | 'expired'

const view = computed<View>(() => {
  if (initializing.value) return 'loading'
  const i = info.value
  if (!i) return 'loadError'
  if (stage.value === 'form') {
    if (i.status === 'custom') return 'custom'
    if (forceCredentials.value || i.status === 'need_client_credentials') return 'credentials'
    return 'idle'
  }
  return stage.value
})

const hasCredentialForm = computed(() => {
  const i = info.value
  return !!i && i.status !== 'custom' && (i.clientIdRequired || i.clientSecret !== 'none' || i.supportsBaseUrl)
})

const remainingMs = computed(() => Math.max(0, expiresAtMs.value - now.value))
const countdownText = computed(() => {
  const total = Math.ceil(remainingMs.value / 1000)
  const m = Math.floor(total / 60)
  const s = total % 60
  return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`
})

const loadErrorText = computed(() => store.error ?? '未找到该提供商的授权信息')

async function load(force = true) {
  initializing.value = true
  if (force || !store.loaded) await store.fetchProviders()
  if (disposed) return
  initializing.value = false
}

function stopTimers() {
  clearTimeout(pollTimer)
  clearTimeout(tickTimer)
  pollTimer = undefined
  tickTimer = undefined
}

function tick() {
  now.value = Date.now()
  if (stage.value !== 'running') return
  if (remainingMs.value <= 0) {
    expire()
    return
  }
  tickTimer = setTimeout(tick, 1000)
}

/** 尽力取消后端会话；错误一律吞掉（会话可能已结束） */
function cancelQuietly(sessionId: string) {
  try {
    void Promise.resolve(store.cancel(sessionId)).catch(() => undefined)
  } catch {
    // 忽略
  }
}

function expire() {
  const sid = session.value?.sessionId
  stopTimers()
  stage.value = 'expired'
  if (sid) cancelQuietly(sid)
}

function fail(code: string) {
  stopTimers()
  errorText.value = oauthErrorText(code)
  stage.value = 'failed'
}

function handleThrown(e: unknown) {
  const code = toOAuthApiError(e).code
  if (code === 'missing_client_credential') {
    // 回到“配置客户端凭据”表单，并刷新能力信息
    stopTimers()
    session.value = null
    forceCredentials.value = true
    errorText.value = null
    stage.value = 'form'
    void store.fetchProviders()
    return
  }
  fail(code)
}

function schedulePoll() {
  clearTimeout(pollTimer)
  pollTimer = setTimeout(pollOnce, POLL_INTERVAL_MS)
}

/** 轮询链：每次请求完成后才安排下一次，请求之间绝不重叠 */
async function pollOnce() {
  const s = session.value
  if (disposed || stage.value !== 'running' || !s) return
  if (Date.now() >= expiresAtMs.value) {
    expire()
    return
  }
  let result
  try {
    result = await store.poll(s.sessionId)
  } catch (e) {
    if (disposed || stage.value !== 'running' || session.value?.sessionId !== s.sessionId) return
    handleThrown(e)
    return
  }
  if (disposed || stage.value !== 'running' || session.value?.sessionId !== s.sessionId) return
  switch (result.status) {
    case 'pending':
      schedulePoll()
      break
    case 'succeeded':
      stopTimers()
      resultConnectionId.value = result.connectionId
      stage.value = 'succeeded'
      emit('done', result.connectionId)
      break
    case 'failed':
      fail(result.code)
      break
    case 'cancelled':
      stopTimers()
      stage.value = 'cancelled'
      break
    case 'expired':
      stopTimers()
      stage.value = 'expired'
      break
  }
}

async function open(url: string) {
  openError.value = null
  try {
    await openExternal(url)
  } catch {
    openError.value = '无法自动打开浏览器，请点击链接重试，或复制地址到浏览器中打开'
  }
}

async function begin() {
  if (stage.value === 'starting' || stage.value === 'running') return
  stopTimers()
  errorText.value = null
  openError.value = null
  stage.value = 'starting'
  try {
    const res = props.reauthConnection
      ? await store.reauthorize(props.reauthConnection.id)
      : await store.start(props.providerId, label.value.trim())
    if (disposed) {
      cancelQuietly(res.sessionId)
      return
    }
    session.value = res
    expiresAtMs.value = Date.parse(res.expiresAt)
    now.value = Date.now()
    stage.value = 'running'
    tickTimer = setTimeout(tick, 1000)
    // authorization-code 与 state-poll 都把授权地址交给系统浏览器打开；device 由用户手动输码。
    if (res.authorizeUrl && (res.flow === 'authorization-code' || res.flow === 'state-poll')) {
      void open(res.authorizeUrl)
    }
    schedulePoll()
  } catch (e) {
    if (disposed) return
    handleThrown(e)
  }
}

function cancelRunning() {
  const sid = session.value?.sessionId
  stopTimers()
  stage.value = 'cancelled'
  if (sid) cancelQuietly(sid)
  emit('cancel')
}

function restart() {
  stopTimers()
  session.value = null
  errorText.value = null
  openError.value = null
  stage.value = 'form'
}

function onCredentialsSaved() {
  forceCredentials.value = false
}

const clearBusy = ref(false)
const clearError = ref<string | null>(null)
async function clearCredentials() {
  if (clearBusy.value) return
  clearBusy.value = true
  clearError.value = null
  try {
    await store.clearClientCredentials(props.providerId)
  } catch (e) {
    clearError.value = oauthErrorText(toOAuthApiError(e).code)
  } finally {
    clearBusy.value = false
  }
}

onMounted(() => load(false))
onBeforeUnmount(() => {
  disposed = true
  const pending = stage.value === 'running' ? session.value?.sessionId : undefined
  stopTimers()
  if (pending) cancelQuietly(pending)
})
</script>

<template>
  <section class="oauth-panel" :aria-label="`${providerName} OAuth ${isReauth ? '重新授权' : '授权'}`">
    <header class="head">
      <h4>{{ isReauth ? 'OAuth 重新授权' : 'OAuth 授权' }}<span v-if="isReauth && reauthConnection" class="who">：{{ reauthConnection.accountLabel }}</span></h4>
    </header>

    <!-- 加载 -->
    <p v-if="view === 'loading'" class="muted" role="status">正在加载授权信息…</p>

    <!-- 加载失败 -->
    <div v-else-if="view === 'loadError'" class="block">
      <p class="err" role="alert">加载授权信息失败：{{ loadErrorText }}</p>
      <div class="actions">
        <button type="button" class="btn" @click="emit('close')">关闭</button>
        <button type="button" class="btn btn-primary" @click="load()">重试</button>
      </div>
    </div>

    <!-- 专有流程：不支持 -->
    <div v-else-if="view === 'custom' && info" class="block">
      <div class="notice neutral" role="note">
        <p><strong>该提供商使用专有登录方式，本版本暂不支持授权。</strong></p>
        <p v-if="info.reason" data-testid="oauth-custom-reason"><strong>原因：</strong>{{ info.reason }}</p>
        <template v-if="info.requires.length">
          <p class="req-title"><strong>接入需要：</strong></p>
          <ul data-testid="oauth-custom-requires">
            <li v-for="r in info.requires" :key="r">{{ r }}</li>
          </ul>
        </template>
      </div>
      <div class="actions">
        <button type="button" class="btn" @click="emit('close')">关闭</button>
        <button type="button" class="btn btn-primary" disabled aria-disabled="true">开始授权</button>
      </div>
    </div>

    <!-- 需要配置客户端凭据 -->
    <div v-else-if="view === 'credentials' && info" class="block">
      <OAuthCredentialForm
        :info="info"
        :provider-name="providerName"
        @saved="onCredentialsSaved"
        @cleared="forceCredentials = false"
        @cancel="emit('close')"
      />
    </div>

    <!-- 空闲：开始授权 -->
    <div v-else-if="view === 'idle' && info" class="block">
      <div v-if="info.riskNotice && !hideRiskNotice" class="notice risk" role="note">
        <AlertTriangle :size="16" aria-hidden="true" />
        <span><strong>风险提示：</strong>{{ info.riskNotice }}</span>
      </div>
      <div v-if="info.limitation" class="notice neutral" role="note" data-testid="oauth-limitation">
        <span><strong>使用限制：</strong>{{ info.limitation }}</span>
      </div>
      <label v-if="!isReauth" class="field">
        <span>账户标识（可选，最多 128 个字符）</span>
        <input
          v-model="label"
          class="input"
          maxlength="128"
          autocomplete="off"
          placeholder="留空则使用授权账号的邮箱或“提供商 账户”"
          aria-label="账户标识"
          @keydown.enter.prevent="begin"
        />
      </label>
      <p v-else class="muted">将为连接“{{ reauthConnection?.accountLabel }}”重新授权，凭据原位更新。</p>
      <p class="muted">点击后会在系统浏览器中打开授权页面，登录并同意后即可完成授权。</p>
      <p v-if="clearError" class="err" role="alert">{{ clearError }}</p>
      <div class="actions">
        <button
          v-if="info.clientConfigured && hasCredentialForm"
          type="button"
          class="btn btn-danger"
          :disabled="clearBusy"
          @click="clearCredentials"
        >
          清除客户端凭据
        </button>
        <span class="spacer" />
        <button type="button" class="btn" @click="emit('close')">取消</button>
        <button type="button" class="btn btn-primary" @click="begin">{{ isReauth ? '开始重新授权' : '开始授权' }}</button>
      </div>
    </div>

    <!-- 启动中 -->
    <p v-else-if="view === 'starting'" class="muted" role="status">正在启动授权…</p>

    <!-- 进行中 -->
    <div v-else-if="view === 'running' && session" class="block">
      <template v-if="session.flow === 'device' && session.userCode && session.verificationUri">
        <DeviceCodeCard :user-code="session.userCode" :verification-uri="session.verificationUri" @open="open" />
        <p class="muted" role="status">正在等待你在浏览器中完成授权…</p>
      </template>
      <template v-else>
        <p class="running" role="status">已在浏览器中打开，请完成登录</p>
        <p v-if="session.authorizeUrl" class="muted">
          没有自动打开？
          <button type="button" class="link" data-testid="oauth-reopen" @click="open(session.authorizeUrl)">
            重新打开授权页面 <ExternalLink :size="12" aria-hidden="true" />
          </button>
        </p>
      </template>
      <p v-if="openError" class="err" role="alert">{{ openError }}</p>
      <p class="muted" data-testid="oauth-countdown">剩余时间 <span class="mono">{{ countdownText }}</span></p>
      <div class="actions">
        <span class="spacer" />
        <button type="button" class="btn" @click="cancelRunning">取消</button>
      </div>
    </div>

    <!-- 成功 -->
    <div v-else-if="view === 'succeeded'" class="block">
      <p class="ok" role="status">授权成功，连接已添加。</p>
      <div v-if="info?.limitation" class="notice neutral" role="note">
        <span><strong>使用限制：</strong>{{ info.limitation }}</span>
      </div>
      <div class="actions">
        <span class="spacer" />
        <button type="button" class="btn btn-primary" @click="emit('close')">完成</button>
      </div>
    </div>

    <!-- 失败 -->
    <div v-else-if="view === 'failed'" class="block">
      <p class="err" role="alert">{{ errorText }}</p>
      <div class="actions">
        <button type="button" class="btn" @click="emit('close')">关闭</button>
        <button type="button" class="btn btn-primary" @click="begin">重试</button>
      </div>
    </div>

    <!-- 已取消 / 已过期 -->
    <div v-else-if="view === 'cancelled' || view === 'expired'" class="block">
      <p class="muted" role="status">{{ view === 'cancelled' ? '授权已取消。' : '授权已过期，请重新开始。' }}</p>
      <div class="actions">
        <button type="button" class="btn" @click="emit('close')">关闭</button>
        <button type="button" class="btn btn-primary" @click="restart">重新开始</button>
      </div>
    </div>
  </section>
</template>

<style scoped>
.oauth-panel { margin: 0 0 12px; padding: 12px 14px; border: 1px solid var(--c-border); border-radius: var(--radius); background: var(--c-sidebar); }
.head h4 { margin: 0 0 8px; font-size: 14px; font-weight: 600; }
.who { font-weight: 400; color: var(--c-text-2); }
.block { display: flex; flex-direction: column; gap: 10px; }
.muted { margin: 0; font-size: 13px; color: var(--c-text-2); }
.running { margin: 0; font-size: 14px; font-weight: 500; }
.ok { margin: 0; font-size: 14px; color: var(--c-success); }
.err { margin: 0; font-size: 13px; color: var(--c-danger); }
.mono { font-family: var(--font-mono); }
.field { display: flex; flex-direction: column; gap: 4px; font-size: 12px; color: var(--c-text-2); }
.notice { display: flex; align-items: flex-start; gap: 8px; padding: 10px 14px; border-radius: var(--radius); font-size: 13px; }
.notice p { margin: 0 0 4px; }
.notice ul { margin: 0; padding-left: 18px; }
.notice.risk { background: #fdf3e0; border: 1px solid #f6d9a3; color: #a35f00; }
.notice.neutral { flex-direction: column; align-items: stretch; gap: 0; background: var(--c-surface-2); color: var(--c-text-2); }
.req-title { margin-top: 4px; }
.actions { display: flex; align-items: center; gap: 8px; justify-content: flex-end; }
.spacer { flex: 1; }
.link { display: inline-flex; align-items: center; gap: 4px; padding: 0; border: 0; background: transparent; color: var(--c-brand); font-size: 13px; }
.link:hover { text-decoration: underline; }
</style>
