<script setup lang="ts">
import { computed, ref } from 'vue'
import DynamicForm from '@/components/media/DynamicForm.vue'
import ModeTabs from '@/components/media/ModeTabs.vue'
import ProviderPicker from '@/components/media/ProviderPicker.vue'
import SubmitRow from '@/components/media/SubmitRow.vue'
import WorkbenchLayout from '@/components/media/WorkbenchLayout.vue'
import { MEDIA_CAPABILITIES } from '@/components/media/capabilities'
import { useMediaPage } from '@/components/media/useMediaPage'

const cap = MEDIA_CAPABILITIES.image
const modeId = ref(cap.modes[0].id)
const fields = computed(() => cap.modes.find((m) => m.id === modeId.value)!.fields)
const { state, busy, providers, providerId, model, values, errors, setValue, submit } = useMediaPage('image', fields)

function onSubmit() {
  void submit({ mode: modeId.value })
}
</script>

<template>
  <WorkbenchLayout kind="image">
    <template #tabs><ModeTabs v-model="modeId" :modes="cap.modes" :disabled="busy" /></template>
    <template #form>
      <form class="form" novalidate @submit.prevent="onSubmit">
        <ProviderPicker v-model:provider-id="providerId" v-model:model="model" :providers="providers" :disabled="busy" />
        <DynamicForm :fields="fields" :values="values" :errors="errors" :provider-id="providerId" :disabled="busy" @change="setValue" />
        <SubmitRow :error="state.submitError">
          <button class="btn btn-primary" type="submit" data-testid="submit" :disabled="busy">
            {{ busy ? '提交中…' : modeId === 'edit' ? '开始编辑' : '生成图片' }}
          </button>
        </SubmitRow>
      </form>
    </template>
  </WorkbenchLayout>
</template>

<style scoped>
.form { display: flex; flex-direction: column; gap: 16px; }
</style>
