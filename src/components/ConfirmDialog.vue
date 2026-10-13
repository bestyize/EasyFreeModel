<script setup lang="ts">
import BaseModal from './BaseModal.vue'

withDefaults(
  defineProps<{
    modelValue: boolean
    title: string
    message: string
    confirmText?: string
    cancelText?: string
    /** 危险操作（删除账户、清空日志、撤销密钥）使用错误色确认按钮 */
    danger?: boolean
  }>(),
  { confirmText: '确认', cancelText: '取消', danger: false },
)
const emit = defineEmits<{ 'update:modelValue': [value: boolean]; confirm: [] }>()

function confirm() {
  emit('confirm')
  emit('update:modelValue', false)
}
</script>

<template>
  <BaseModal :model-value="modelValue" :title="title" width="420px" @update:model-value="emit('update:modelValue', $event)">
    <p class="message">{{ message }}</p>
    <template #footer>
      <button class="btn" type="button" @click="emit('update:modelValue', false)">{{ cancelText }}</button>
      <button class="btn btn-primary" type="button" :class="{ danger }" @click="confirm">{{ confirmText }}</button>
    </template>
  </BaseModal>
</template>

<style scoped>
.message { margin: 0; color: var(--c-text-2); line-height: 1.6; }
.danger { background: var(--c-danger); border-color: var(--c-danger); }
.danger:hover { background: #cf3b3b; }
</style>
