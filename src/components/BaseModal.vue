<script setup lang="ts">
import { nextTick, onBeforeUnmount, ref, watch } from 'vue'
import { X } from 'lucide-vue-next'
import { isTopModal, pushModal, removeModal } from './modalStack'

const modalId = Symbol('modal')

const props = withDefaults(
  defineProps<{
    modelValue: boolean
    title?: string
    /** 弹窗宽度，默认 860px，最大 calc(100vw - 64px) */
    width?: string
    /** 有未保存修改时，ESC/遮罩/关闭按钮会先询问 */
    dirty?: boolean
    closeOnMask?: boolean
  }>(),
  { title: '', width: '860px', dirty: false, closeOnMask: true },
)
const emit = defineEmits<{ 'update:modelValue': [value: boolean]; close: [] }>()

const dialogEl = ref<HTMLElement | null>(null)
const askingDiscard = ref(false)
let previouslyFocused: HTMLElement | null = null

function close() {
  askingDiscard.value = false
  emit('update:modelValue', false)
  emit('close')
}

function requestClose() {
  if (props.dirty) {
    askingDiscard.value = true
    return
  }
  close()
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') {
    if (!isTopModal(modalId)) return
    e.stopPropagation()
    if (askingDiscard.value) askingDiscard.value = false
    else requestClose()
  }
}

watch(
  () => props.modelValue,
  async (open) => {
    if (open) {
      previouslyFocused = document.activeElement as HTMLElement | null
      pushModal(modalId)
      document.addEventListener('keydown', onKeydown, true)
      await nextTick()
      dialogEl.value?.focus()
    } else {
      removeModal(modalId)
      document.removeEventListener('keydown', onKeydown, true)
      previouslyFocused?.focus?.()
    }
  },
  { immediate: true },
)
onBeforeUnmount(() => {
  removeModal(modalId)
  document.removeEventListener('keydown', onKeydown, true)
})

defineExpose({ requestClose, close })
</script>

<template>
  <Teleport to="body">
    <div v-if="modelValue" class="modal-mask" @mousedown.self="closeOnMask && requestClose()">
      <div
        ref="dialogEl"
        class="modal"
        role="dialog"
        aria-modal="true"
        :aria-label="title || undefined"
        tabindex="-1"
        :style="{ width }"
      >
        <header class="modal-header">
          <slot name="header">
            <h2>{{ title }}</h2>
          </slot>
          <button class="close" type="button" aria-label="关闭" @click="requestClose"><X :size="20" /></button>
        </header>

        <div class="modal-body"><slot /></div>

        <footer v-if="$slots.footer" class="modal-footer"><slot name="footer" /></footer>

        <div v-if="askingDiscard" class="discard" role="alertdialog" aria-label="放弃未保存的修改">
          <div class="discard-box card">
            <p>有未保存的修改，确定要放弃吗？</p>
            <div class="row">
              <button class="btn" type="button" @click="askingDiscard = false">继续编辑</button>
              <button class="btn btn-primary" type="button" @click="close">放弃修改</button>
            </div>
          </div>
        </div>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.modal-mask { position: fixed; inset: 0; z-index: 1000; display: grid; place-items: center; background: var(--c-overlay); }
.modal { position: relative; display: flex; flex-direction: column; max-width: calc(100vw - 64px); max-height: calc(100vh - 80px); background: var(--c-canvas); border-radius: 14px; outline: none; overflow: hidden; }
.modal-header { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 18px 24px; flex: none; }
.modal-header h2 { margin: 0; font-size: var(--fs-section); font-weight: 600; }
.close { display: grid; place-items: center; width: 32px; height: 32px; border: 0; border-radius: 8px; background: transparent; color: var(--c-text-2); }
.close:hover { background: var(--c-surface-2); }
.modal-body { flex: 1; min-height: 0; overflow-y: auto; padding: 0 24px 16px; }
.modal-footer { display: flex; justify-content: flex-end; gap: 10px; padding: 14px 24px; border-top: 1px solid var(--c-border); flex: none; }
.discard { position: absolute; inset: 0; display: grid; place-items: center; background: var(--c-overlay); }
.discard-box { padding: 20px 24px; min-width: 320px; }
.discard-box p { margin: 0 0 16px; }
.row { display: flex; justify-content: flex-end; gap: 10px; }
</style>
