/**
 * OAuth 命令的错误模型。Rust 侧以 `{ code, message }` 拒绝（message 是固定的安全文案，
 * 不含服务端原始文本、令牌或凭据）；界面只依据 code 映射中文文案，不显示原始文本。
 */

export class OAuthApiError extends Error {
  readonly code: string
  constructor(code: string, message?: string) {
    super(message ?? oauthErrorText(code))
    this.name = 'OAuthApiError'
    this.code = code
  }
}

const CODE_PATTERN = /^[a-z][a-z0-9_]{0,63}$/

/** 把 invoke 的拒绝值（`{code,message}` 对象 / Error / 字符串）规范成 OAuthApiError */
export function toOAuthApiError(e: unknown): OAuthApiError {
  if (e instanceof OAuthApiError) return e
  if (e && typeof e === 'object' && 'code' in e) {
    const code = String((e as { code: unknown }).code)
    return new OAuthApiError(CODE_PATTERN.test(code) ? code : 'internal')
  }
  return new OAuthApiError('internal')
}

const TEXT: Record<string, string> = {
  unknown_provider: '未知的授权提供商',
  custom_flow_required: '该提供商使用专有登录方式，本版本暂不支持',
  unsupported_flow: '该提供商的授权方式与请求不符',
  missing_client_credential: '需要配置客户端凭据',
  config: '授权配置无效',
  state_mismatch: '安全校验失败（state 不匹配），授权已被拒绝',
  access_denied: '授权被拒绝：你拒绝了授权，或授权服务拒绝了本次请求',
  callback_error: '授权服务返回了错误，请重试',
  missing_code: '授权回调缺少授权码，请重试',
  timeout: '等待授权超时，请重新开始',
  expired: '授权已过期，请重新开始',
  device_code_expired: '设备码已过期，请重新开始',
  cancelled: '授权已取消',
  listen_failed: '无法在本机监听授权回调端口（可能被其它程序占用），请关闭占用后重试',
  token_endpoint: '授权服务拒绝了令牌请求，请重试',
  reauth_required: '需要重新授权',
  transient: '网络异常或授权服务暂时不可用，请稍后重试',
  invalid_response: '授权服务返回了无法识别的响应',
  no_tokens: '没有可用的授权令牌，请重新授权',
  storage: '无法读写系统凭据库，授权结果未保存',
  too_many_sessions: '进行中的授权过多，请先完成或取消其中一些',
  label_invalid: '账户标识需为 1–128 个字符，且不含控制字符',
  invalid_input: '输入无效，请检查后重试',
  invalid_callback: '回调地址与本次授权不匹配，请复制浏览器地址栏中的完整地址',
  not_found: '找不到对应的连接',
  not_oauth: '该连接不是 OAuth 连接',
  internal: '内部错误，请重试',
}

/** 错误码 -> 中文文案；未知错误码给出通用文案并附带错误码（错误码本身是脱敏的安全字符集） */
export function oauthErrorText(code: string): string {
  return TEXT[code] ?? `授权失败（错误码 ${CODE_PATTERN.test(code) ? code : 'unknown'}）`
}
