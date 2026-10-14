<script setup lang="ts">
import { ref, watch } from 'vue'
import { Check, Copy } from 'lucide-vue-next'
import BaseModal from '@/components/BaseModal.vue'

const props = defineProps<{
  modelValue: boolean
  secret: string
  keyName: string
  mode: 'create' | 'rotate' | 'view'
}>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()

const copied = ref(false)
const copyError = ref<string | null>(null)

watch(
  () => props.modelValue,
  (open) => {
    if (!open) {
      copied.value = false
      copyError.value = null
    }
  },
)

async function copy() {
  copyError.value = null
  try {
    await navigator.clipboard.writeText(props.secret)
    copied.value = true
  } catch {
    copied.value = false
    copyError.value = '复制失败，请手动选中上方密钥后复制'
  }
}
</script>

<template>
  <BaseModal
    :model-value="modelValue"
    :title="mode === 'view' ? '查看密钥' : mode === 'create' ? '密钥已创建' : '密钥已轮换'"
    width="560px"
    @update:model-value="emit('update:modelValue', $event)"
  >
    <p class="lead">
      「{{ keyName }}」的完整密钥如下。<template v-if="mode === 'rotate'">旧密钥已立即失效。</template>
    </p>
    <div class="tip-box" role="status">
      <span>完整密钥已保存在本地数据库中，您可以随时在此复制或在密钥列表中查看。</span>
    </div>
    <code class="secret" data-testid="secret-value" aria-label="完整密钥">{{ secret }}</code>
    <div class="copy-row">
      <button class="btn btn-primary" type="button" data-testid="copy-secret" @click="copy">
        <component :is="copied ? Check : Copy" :size="14" aria-hidden="true" />
        {{ copied ? '已复制' : '复制完整密钥' }}
      </button>
      <span v-if="copyError" class="copy-error" role="alert">{{ copyError }}</span>
    </div>
    <template #footer>
      <button class="btn" type="button" data-testid="close-secret" @click="emit('update:modelValue', false)">
        关闭
      </button>
    </template>
  </BaseModal>
</template>

<style scoped>
.lead { margin: 0 0 12px; color: var(--c-text-2); line-height: 1.6; }
.tip-box { display: flex; align-items: flex-start; gap: 8px; padding: 10px 12px; margin-bottom: 12px; border-radius: 8px; background: var(--c-surface-2); color: var(--c-text); font-size: var(--fs-small); line-height: 1.5; border: 1px solid var(--c-border); }
.secret { display: block; padding: 12px; border: 1px solid var(--c-border); border-radius: 8px; background: var(--c-surface-2); font-family: var(--font-mono); font-size: var(--fs-small); word-break: break-all; user-select: all; }
.copy-row { display: flex; align-items: center; gap: 12px; margin-top: 12px; }
.copy-error { color: var(--c-danger); font-size: var(--fs-small); }
</style>
