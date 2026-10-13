import type { Api } from './types'
import { tauriApi } from './tauri'
import { mockApi } from './mock'

const inTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

export const api: Api = inTauri ? tauriApi : mockApi
export type { Api } from './types'
