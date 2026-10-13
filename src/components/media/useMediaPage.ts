import { computed, onActivated, onBeforeUnmount, onDeactivated, onMounted, reactive, ref, watch, type Ref } from 'vue'
import { useMediaStore } from '@/stores/media'
import type { MediaKind } from '@/types'
import { fieldState, type FieldDef } from './capabilities'
import { buildParams, defaultValues, validateField, validateFields, type FormErrors, type FormValues } from './forms'

/** 多模态页面共用逻辑：加载供应商 / 历史、轮询生命周期、供应商选择、表单校验与提交。 */
export function useMediaPage(kind: MediaKind, fields: Readonly<Ref<FieldDef[]>>) {
  const media = useMediaStore()

  const providerId = ref('')
  const model = ref('')
  const state = computed(() => media.kinds[kind])
  const job = computed(() => media.currentJob(kind))
  const providers = computed(() => media.availableProviders)
  const preparing = ref(false)
  const busy = computed(() => preparing.value || state.value.submitting)

  watch(
    providers,
    (list) => {
      if (!list.some((p) => p.id === providerId.value)) providerId.value = list[0]?.id ?? ''
    },
    { immediate: true },
  )

  const values = reactive<FormValues>({})
  const errors = ref<FormErrors>({})
  let attempted = false

  /** keep=true 时（切换二级 Tab）保留同名字段已填的值，如提示词 */
  function resetValues(fs: FieldDef[], keep = false) {
    const next = defaultValues(fs)
    if (keep) for (const k of Object.keys(next)) if (k in values) next[k] = values[k]
    for (const k of Object.keys(values)) delete values[k]
    Object.assign(values, next)
    errors.value = {}
    attempted = false
  }
  watch(fields, (fs) => resetValues(fs, true), { immediate: true })

  function setValue(key: string, value: FormValues[string]) {
    values[key] = value
    const f = fields.value.find((x) => x.key === key)
    // 文件字段选择后立即校验；其它字段在首次提交失败后实时校验
    if (f && (attempted || f.type === 'file') && fieldState(f, providerId.value) === 'enabled') {
      const err = validateField(f, value)
      const next = { ...errors.value }
      if (err) next[key] = err
      else delete next[key]
      errors.value = next
    }
  }

  function validate(): boolean {
    attempted = true
    errors.value = validateFields(fields.value, values, providerId.value)
    return Object.keys(errors.value).length === 0
  }

  /** 校验通过后提交；重复提交由 store 忽略，按钮另在 UI 层禁用。 */
  async function submit(extra: Record<string, unknown> = {}, extraCheck?: () => boolean): Promise<boolean> {
    if (busy.value) return false
    const ok = validate()
    const extraOk = extraCheck ? extraCheck() : true
    if (!ok || !extraOk) return false
    if (!providerId.value) {
      state.value.submitError = '请先选择可用的供应商'
      return false
    }
    let params: Record<string, unknown>
    // 读取文件是异步的，期间也视为“提交中”，避免重复点击
    preparing.value = true
    try {
      params = await buildParams(fields.value, values, providerId.value)
    } catch (e) {
      state.value.submitError = e instanceof Error ? e.message : String(e)
      return false
    } finally {
      preparing.value = false
    }
    const job = await media.submit(kind, {
      ...params,
      ...extra,
      providerId: providerId.value,
      ...(model.value.trim() ? { model: model.value.trim() } : {}),
    })
    return job !== null
  }

  function refresh() {
    void media.loadProviders()
    void media.loadJobs(kind)
  }

  let activatedOnce = false
  onMounted(refresh)
  onActivated(() => {
    // KeepAlive 首次激活紧随 mounted，已经加载
    if (!activatedOnce) {
      activatedOnce = true
      return
    }
    refresh()
  })
  onDeactivated(() => media.stopPolling(kind))
  onBeforeUnmount(() => media.stopPolling(kind))

  return { media, state, busy, job, providers, providerId, model, values, errors, setValue, validate, submit, refresh, resetValues }
}
