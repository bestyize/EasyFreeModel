export interface FilePayload {
  name: string
  size: number
  mimeType: string
  /** 仅用于经 IPC 交给后端的瞬时载荷，不得写入日志 */
  dataBase64: string
}

export function fileExtension(name: string): string {
  const i = name.lastIndexOf('.')
  return i >= 0 ? name.slice(i + 1).toLowerCase() : ''
}

export function readFileAsBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onerror = () => reject(new Error(`读取文件失败：${file.name}`))
    reader.onload = () => {
      const result = String(reader.result ?? '')
      const comma = result.indexOf(',')
      resolve(comma >= 0 ? result.slice(comma + 1) : result)
    }
    reader.readAsDataURL(file)
  })
}

export async function toFilePayload(file: File): Promise<FilePayload> {
  return { name: file.name, size: file.size, mimeType: file.type, dataBase64: await readFileAsBase64(file) }
}
