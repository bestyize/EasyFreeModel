<script setup lang="ts">
import { computed, ref } from 'vue'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import { useSettingsStore } from '@/stores/settings'
import SettingsRow from './SettingsRow.vue'
import ToggleSwitch from './ToggleSwitch.vue'

export interface DataCapabilities {
  backup: boolean
  restore: boolean
  clearRecords: boolean
}

/** 契约（SystemApi）目前没有备份 / 恢复 / 清空接口，默认全部不可用并标注“待实现”。 */
withDefaults(defineProps<{ capabilities?: DataCapabilities }>(), {
  capabilities: () => ({ backup: false, restore: false, clearRecords: false }),
})
const emit = defineEmits<{ backup: []; restore: []; 'clear-records': [] }>()

const settings = useSettingsStore()
const draft = computed(() => settings.draft!)
const clearOpen = ref(false)
const pendingHint = '待实现：后端尚未提供该能力'

</script>

<template>
  <section class="card section" aria-labelledby="set-data">
    <h2 id="set-data" class="section-title">数据与安全</h2>

    <SettingsRow label="密钥存储" description="第三方 API Key / OAuth 令牌由 Rust 后端保存到系统凭据库（Keychain / Credential Manager），前端不持有明文。">
      <span class="muted">状态未知（后端暂未提供查询接口）</span>
    </SettingsRow>
    <SettingsRow label="日志脱敏" description="开启后日志不记录请求正文、图片 base64、Authorization、Cookie 与令牌。">
      <ToggleSwitch v-model="draft.redactLogs" label="日志脱敏" />
    </SettingsRow>
    <SettingsRow label="备份" description="导出本地配置与账户元数据；导出文件可能包含敏感信息。">
      <span v-if="!capabilities.backup" class="pending">待实现</span>
      <button class="btn" type="button" data-testid="btn-backup" :disabled="!capabilities.backup" :title="capabilities.backup ? undefined : pendingHint" @click="emit('backup')">
        备份数据
      </button>
    </SettingsRow>
    <SettingsRow label="恢复" description="从备份文件恢复，会覆盖当前配置。">
      <span v-if="!capabilities.restore" class="pending">待实现</span>
      <button class="btn" type="button" data-testid="btn-restore" :disabled="!capabilities.restore" :title="capabilities.restore ? undefined : pendingHint" @click="emit('restore')">
        恢复数据
      </button>
    </SettingsRow>
    <SettingsRow label="清空记录" description="删除请求日志与使用统计，无法恢复。">
      <span v-if="!capabilities.clearRecords" class="pending">待实现</span>
      <button
        class="btn btn-danger" type="button" data-testid="btn-clear" :disabled="!capabilities.clearRecords"
        :title="capabilities.clearRecords ? undefined : pendingHint" @click="clearOpen = true"
      >
        清空记录
      </button>
    </SettingsRow>

    <ConfirmDialog
      v-model="clearOpen"
      title="清空记录"
      message="将删除全部请求日志与使用统计，此操作无法撤销。确定继续吗？"
      confirm-text="确认清空"
      danger
      @confirm="emit('clear-records')"
    />
  </section>
</template>

<style scoped>
.section { padding: 20px 24px; }
.muted { color: var(--c-text-2); font-size: var(--fs-small); }
.pending { padding: 2px 8px; border-radius: 4px; background: var(--c-surface-2); color: var(--c-text-2); font-size: var(--fs-caption); }
</style>
