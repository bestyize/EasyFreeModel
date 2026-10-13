<script setup lang="ts">
import { computed } from 'vue'
import DynamicForm from '@/components/media/DynamicForm.vue'
import ProviderPicker from '@/components/media/ProviderPicker.vue'
import SearchResults from '@/components/media/SearchResults.vue'
import SubmitRow from '@/components/media/SubmitRow.vue'
import WorkbenchLayout from '@/components/media/WorkbenchLayout.vue'
import { MEDIA_CAPABILITIES } from '@/components/media/capabilities'
import { useMediaPage } from '@/components/media/useMediaPage'

const fields = computed(() => MEDIA_CAPABILITIES.search.modes[0].fields)
const { state, busy, providers, providerId, model, values, errors, setValue, submit } = useMediaPage('search', fields)

function onSubmit() {
  void submit()
}
</script>

<template>
  <WorkbenchLayout kind="search" hide-history-summary>
    <template #form>
      <form class="form" novalidate @submit.prevent="onSubmit">
        <ProviderPicker v-model:provider-id="providerId" v-model:model="model" :providers="providers" :disabled="busy" />
        <DynamicForm :fields="fields" :values="values" :errors="errors" :provider-id="providerId" :disabled="busy" @change="setValue" />
        <SubmitRow :error="state.submitError">
          <button class="btn btn-primary" type="submit" data-testid="submit" :disabled="busy">
            {{ busy ? '搜索中…' : '搜索' }}
          </button>
        </SubmitRow>
      </form>
    </template>
    <template #result="{ job }"><SearchResults :summary="job.summary" /></template>
  </WorkbenchLayout>
</template>

<style scoped>
.form { display: flex; flex-direction: column; gap: 16px; }
</style>
