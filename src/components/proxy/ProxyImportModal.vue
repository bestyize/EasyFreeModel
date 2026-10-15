<script setup lang="ts">
import { ref, watch } from 'vue'
import BaseModal from '@/components/BaseModal.vue'
import { errorMessage } from '@/components/media/format'
import { useProxyPoolStore } from '@/stores/proxyPool'
import type { ProxyImportResult } from '@/types'

const props = defineProps<{ modelValue: boolean }>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()

const store = useProxyPoolStore()
const text = ref('')
const submitting = ref(false)
const error = ref<string | null>(null)
const result = ref<ProxyImportResult | null>(null)

watch(
  () => props.modelValue,
  () => {
    text.value = ''
    error.value = null
    result.value = null
    submitting.value = false
  },
)

async function submit() {
  if (submitting.value || !text.value.trim()) return
  submitting.value = true
  error.value = null
  try {
    result.value = await store.importNodes(text.value)
    // 导入完成后立刻清掉输入框，避免明文账号密码继续留在界面里
    text.value = ''
  } catch (e) {
    error.value = `导入失败：${errorMessage(e)}`
  } finally {
    submitting.value = false
  }
}
</script>

<template>
  <BaseModal :model-value="modelValue" title="批量导入代理" width="620px" @update:model-value="emit('update:modelValue', $event)">
    <div class="body">
      <template v-if="!result">
        <p class="lead">每行一个代理，空行和以 # 开头的行会被忽略。支持以下写法：</p>
        <pre class="formats">socks5://user:pass@10.0.0.1:1080
http://proxy.example.com:8080
10.0.0.2:3128
10.0.0.3:3128:user:pass</pre>
        <textarea
          v-model="text" class="input area" rows="8" spellcheck="false" aria-label="代理列表" data-testid="import-text"
          placeholder="粘贴代理列表…"
        />
        <p class="hint">没有写协议时按 HTTP 处理；与已有节点（协议、主机、端口、用户名都相同）重复的会跳过。</p>
        <p v-if="error" class="err" role="alert">{{ error }}</p>
      </template>
      <template v-else>
        <p class="summary" data-testid="import-summary">已导入 {{ result.added }} 个节点，跳过 {{ result.skipped.length }} 行。</p>
        <ul v-if="result.skipped.length" class="skipped" data-testid="import-skipped">
          <li v-for="s in result.skipped" :key="s.line">第 {{ s.line }} 行：{{ s.reason }}</li>
        </ul>
        <p class="hint">导入的节点还没有测速，可以在列表里点“全部测速”确认可用性。</p>
      </template>
    </div>
    <template #footer>
      <template v-if="!result">
        <button class="btn" type="button" @click="emit('update:modelValue', false)">取消</button>
        <button class="btn btn-primary" type="button" data-testid="submit-import" :disabled="submitting || !text.trim()" @click="submit">
          {{ submitting ? '导入中…' : '导入' }}
        </button>
      </template>
      <button v-else class="btn btn-primary" type="button" @click="emit('update:modelValue', false)">完成</button>
    </template>
  </BaseModal>
</template>

<style scoped>
.body { display: flex; flex-direction: column; gap: 12px; }
.lead, .summary { margin: 0; font-size: var(--fs-small); }
.summary { font-weight: 600; }
.formats { margin: 0; padding: 10px 12px; border-radius: 8px; background: var(--c-surface-2); font-family: var(--font-mono); font-size: var(--fs-caption); line-height: 1.7; }
.area { height: auto; padding: 8px 12px; resize: vertical; font-family: var(--font-mono); }
.hint { margin: 0; color: var(--c-text-2); font-size: var(--fs-caption); }
.err { margin: 0; color: var(--c-danger); font-size: var(--fs-small); }
.skipped { margin: 0; padding: 10px 12px 10px 28px; max-height: 200px; overflow-y: auto; border-radius: 8px; background: var(--c-warning-soft); color: var(--c-warning); font-size: var(--fs-caption); line-height: 1.7; }
</style>
