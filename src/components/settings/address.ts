/** 判断监听地址是否为回环地址。空值按“非回环”处理（等同未限制绑定）。 */
export function isLoopbackAddress(address: string | null | undefined): boolean {
  const a = (address ?? '').trim().toLowerCase()
  if (!a) return false
  if (a === 'localhost' || a === '::1' || a === '[::1]') return true
  return /^127(\.\d{1,3}){3}$/.test(a)
}

/** 粗略校验监听地址：localhost / IPv4 / IPv6（含方括号形式）。 */
export function isValidListenAddress(address: string): boolean {
  const a = address.trim()
  if (!a) return false
  if (a.toLowerCase() === 'localhost') return true
  const v4 = a.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/)
  if (v4) return v4.slice(1).every((n) => Number(n) <= 255)
  const v6 = a.replace(/^\[|\]$/g, '')
  return /^[0-9a-f:]+$/i.test(v6) && v6.includes(':')
}
