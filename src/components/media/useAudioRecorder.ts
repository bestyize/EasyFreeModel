import { onBeforeUnmount, ref } from 'vue'

export function describeRecordError(e: unknown): string {
  const name = e && typeof e === 'object' && 'name' in e ? String((e as { name: unknown }).name) : ''
  switch (name) {
    case 'NotAllowedError':
    case 'SecurityError':
    case 'PermissionDeniedError':
      return '麦克风权限被拒绝。请在系统设置或浏览器权限中允许本应用使用麦克风后重试。'
    case 'NotFoundError':
    case 'OverconstrainedError':
      return '未检测到可用的麦克风设备，请检查是否已连接。'
    case 'NotReadableError':
    case 'AbortError':
      return '麦克风无法读取，可能被其他程序占用。'
    default: {
      const msg = e instanceof Error ? e.message : ''
      return `无法开始录音${msg ? `：${msg}` : ''}`
    }
  }
}

function extensionFor(mime: string): string {
  if (mime.includes('ogg')) return 'ogg'
  if (mime.includes('mp4')) return 'm4a'
  if (mime.includes('wav')) return 'wav'
  return 'webm'
}

/** 使用系统麦克风录音。必须由用户点击触发，权限失败给出明确提示。 */
export function useAudioRecorder(opts: { maxSeconds: number; onFinish: (file: File) => void }) {
  const recording = ref(false)
  const seconds = ref(0)
  const error = ref<string | null>(null)

  let recorder: MediaRecorder | null = null
  let stream: MediaStream | null = null
  let timer: ReturnType<typeof setInterval> | undefined
  let chunks: Blob[] = []

  function cleanup() {
    if (timer) clearInterval(timer)
    timer = undefined
    stream?.getTracks().forEach((t) => t.stop())
    stream = null
    recording.value = false
  }

  async function start() {
    if (recording.value) return
    error.value = null
    if (typeof navigator === 'undefined' || !navigator.mediaDevices?.getUserMedia || typeof MediaRecorder === 'undefined') {
      error.value = '当前环境不支持麦克风录音，请改用上传音频文件。'
      return
    }
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: true })
    } catch (e) {
      error.value = describeRecordError(e)
      return
    }
    try {
      chunks = []
      const rec = new MediaRecorder(stream)
      recorder = rec
      rec.ondataavailable = (ev) => {
        if (ev.data.size > 0) chunks.push(ev.data)
      }
      rec.onerror = () => {
        error.value = '录音过程中出错，已停止。'
        cleanup()
      }
      rec.onstop = () => {
        const mime = rec.mimeType || 'audio/webm'
        const blob = new Blob(chunks, { type: mime })
        chunks = []
        cleanup()
        if (blob.size === 0) {
          error.value = '没有录到任何声音，请重试。'
          return
        }
        const stamp = new Date().toISOString().replace(/[:.]/g, '-')
        opts.onFinish(new File([blob], `recording-${stamp}.${extensionFor(mime)}`, { type: mime }))
      }
      rec.start()
      seconds.value = 0
      recording.value = true
      timer = setInterval(() => {
        seconds.value += 1
        if (seconds.value >= opts.maxSeconds) stop()
      }, 1000)
    } catch (e) {
      cleanup()
      error.value = describeRecordError(e)
    }
  }

  function stop() {
    if (recorder && recorder.state !== 'inactive') recorder.stop()
    else cleanup()
  }

  onBeforeUnmount(() => {
    if (recorder && recorder.state !== 'inactive') {
      recorder.onstop = null
      recorder.stop()
    }
    cleanup()
  })

  return { recording, seconds, error, start, stop }
}
