<script setup lang="ts">
import { onBeforeUnmount, ref } from 'vue'
import { Copy, ExternalLink } from 'lucide-vue-next'

/** 设备码展示：user_code 用大号等宽字体，可复制；验证页面通过 open 事件交给父组件用系统浏览器打开 */
const props = defineProps<{ userCode: string; verificationUri: string }>()
const emit = defineEmits<{ open: [url: string] }>()

const feedback = ref<'idle' | 'copied' | 'failed'>('idle')
let timer: ReturnType<typeof setTimeout> | undefined

async function copy() {
  try {
    await navigator.clipboard.writeText(props.userCode)
    feedback.value = 'copied'
  } catch {
    feedback.value = 'failed'
  }
  clearTimeout(timer)
  timer = setTimeout(() => (feedback.value = 'idle'), 2000)
}
onBeforeUnmount(() => clearTimeout(timer))
</script>

<template>
  <div class="device">
    <p class="step">1. 在浏览器中打开下面的验证页面，并输入设备码：</p>
    <div class="code-row">
      <code class="user-code" data-testid="oauth-user-code" aria-label="设备码">{{ userCode }}</code>
      <button type="button" class="btn" aria-label="复制设备码" @click="copy">
        <Copy :size="14" aria-hidden="true" /> 复制
      </button>
      <span class="copy-msg" :class="{ bad: feedback === 'failed' }" role="status">
        <template v-if="feedback === 'copied'">已复制</template>
        <template v-else-if="feedback === 'failed'">复制失败，请手动选择设备码</template>
      </span>
    </div>
    <p class="step">2.
      <button type="button" class="link" data-testid="oauth-verification-link" @click="emit('open', verificationUri)">
        {{ verificationUri }} <ExternalLink :size="12" aria-hidden="true" />
      </button>
    </p>
  </div>
</template>

<style scoped>
.device { display: flex; flex-direction: column; gap: 10px; }
.step { margin: 0; font-size: 13px; color: var(--c-text-2); }
.code-row { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
.user-code {
  padding: 8px 18px;
  border: 1px solid var(--c-border);
  border-radius: var(--radius);
  background: var(--c-surface-2);
  color: var(--c-text);
  font-family: var(--font-mono);
  font-size: 28px;
  font-weight: 600;
  letter-spacing: 0.12em;
  user-select: all;
}
.copy-msg { font-size: 12px; color: var(--c-success); }
.copy-msg.bad { color: var(--c-danger); }
.link { display: inline-flex; align-items: center; gap: 4px; padding: 0; border: 0; background: transparent; color: var(--c-brand); font-size: 13px; word-break: break-all; text-align: left; }
.link:hover { text-decoration: underline; }
</style>
