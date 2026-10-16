<script setup lang="ts">
import { ArrowUpCircle, X } from 'lucide-vue-next'
import { useUpdateStore } from '@/stores/update'

const update = useUpdateStore()
</script>

<template>
  <div v-if="update.noticeVisible" class="update-notice" role="status" aria-label="发现新版本">
    <ArrowUpCircle :size="17" aria-hidden="true" />
    <span>FreeApiBox v{{ update.latestVersion }} 已发布</span>
    <button class="btn" type="button" @click="update.download">前往下载</button>
    <button class="dismiss" type="button" aria-label="暂不更新" title="暂不更新" @click="update.dismiss"><X :size="16" /></button>
  </div>
</template>

<style scoped>
.update-notice { display: flex; align-items: center; flex: none; gap: 10px; padding: 9px var(--page-pad-x); color: var(--c-brand); background: var(--c-brand-soft); border-bottom: 1px solid var(--c-border); font-size: var(--fs-small); }
.update-notice .btn { margin-left: auto; }
.dismiss { display: grid; place-items: center; width: 28px; height: 28px; padding: 0; border: 0; border-radius: 6px; background: transparent; color: var(--c-text-2); }
.dismiss:hover { background: var(--c-surface-2); }
</style>
