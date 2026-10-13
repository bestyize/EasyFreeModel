import type { MediaApi } from '../types'

/**
 * 浏览器预览 mock：不伪造任何生成结果。历史为空，提交一律报“适配器尚未接入”，
 * 与当前后端（媒体适配器尚未实现）的真实状态一致。
 */
export const mediaMock: MediaApi = {
  listMediaJobs: async () => [],
  submitMediaJob: async () => {
    throw new Error('后端媒体适配器尚未接入，无法提交任务')
  },
  cancelMediaJob: async () => {
    throw new Error('任务不存在或后端媒体适配器尚未接入')
  },
}
