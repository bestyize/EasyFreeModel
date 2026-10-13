import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import { defineComponent, h, ref } from 'vue'
import BaseModal from './BaseModal.vue'

const Nested = defineComponent({
  setup() {
    const outer = ref(true)
    const inner = ref(true)
    return () =>
      h('div', [
        h(BaseModal, { modelValue: outer.value, 'onUpdate:modelValue': (v: boolean) => (outer.value = v), title: 'outer' }, () => 'outer body'),
        h(BaseModal, { modelValue: inner.value, 'onUpdate:modelValue': (v: boolean) => (inner.value = v), title: 'inner' }, () => 'inner body'),
        h('output', { id: 'state' }, `${outer.value}|${inner.value}`),
      ])
  },
})

function esc() {
  document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
}

describe('BaseModal ESC handling', () => {
  it('closes only the topmost modal when modals are stacked', async () => {
    const wrapper = mount(Nested, { attachTo: document.body })
    await wrapper.vm.$nextTick()

    esc()
    await wrapper.vm.$nextTick()
    expect(wrapper.find('#state').text()).toBe('true|false')

    esc()
    await wrapper.vm.$nextTick()
    expect(wrapper.find('#state').text()).toBe('false|false')

    wrapper.unmount()
  })
})
