<script setup lang="ts">
// 定价编辑器浮窗：每个实例自持草稿，向窗口栈上报脏状态以供淘汰判定。
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { Price } from '@/api/types';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { formatUsdAmount, parseUsdToMicros } from '@/lib/format';
import type { FieldValidationSpec } from '@/lib/form-validation';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const props = withDefaults(
  defineProps<{
    /** 编辑对象；null 表示新建。 */
    initial: Price | null;
    anchor?: FloatingWindowAnchor | null;
    stackOrder?: number;
    /** 初始位置级联偏移序号。 */
    cascade?: number;
    attention?: boolean;
    topmost?: boolean;
  }>(),
  { anchor: null, stackOrder: 0, cascade: 0, attention: false, topmost: true },
);

const emit = defineEmits<{
  close: [];
  raise: [];
  'dirty-change': [dirty: boolean];
}>();

const { t } = useI18n();

const uid = useId();
const modelInputId = `pricing-editor-model-${uid}`;
const inputInputId = `pricing-editor-input-${uid}`;
const outputInputId = `pricing-editor-output-${uid}`;
const cacheReadInputId = `pricing-editor-cache-read-${uid}`;
const cacheWriteInputId = `pricing-editor-cache-write-${uid}`;

const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();

function formatOptional(micros: number | null): string {
  return micros === null ? '' : formatUsdAmount(micros);
}

const initialValues = {
  model: props.initial?.model ?? '',
  input: props.initial ? formatUsdAmount(props.initial.input_micros) : '',
  output: props.initial ? formatUsdAmount(props.initial.output_micros) : '',
  cacheRead: props.initial ? formatOptional(props.initial.cache_read_micros) : '',
  cacheWrite: props.initial ? formatOptional(props.initial.cache_write_micros) : '',
};

const editorModel = ref(initialValues.model);
const editorInput = ref(initialValues.input);
const editorOutput = ref(initialValues.output);
const editorCacheRead = ref(initialValues.cacheRead);
const editorCacheWrite = ref(initialValues.cacheWrite);
const editorError = ref('');

const dirty = computed(
  () =>
    editorModel.value !== initialValues.model ||
    editorInput.value !== initialValues.input ||
    editorOutput.value !== initialValues.output ||
    editorCacheRead.value !== initialValues.cacheRead ||
    editorCacheWrite.value !== initialValues.cacheWrite,
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const saveMutation = useMutation({
  mutationFn: (body: Price) =>
    props.initial === null
      ? apiClient.createPrice(body)
      : apiClient.updatePrice(initialValues.model, body),
  onSuccess: async () => {
    editorError.value = '';
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['prices'] });
  },
  onError: (err) => {
    editorError.value = extractApiError(err).message;
  },
});

function optionalMicros(value: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  return parseUsdToMicros(trimmed);
}

function handleSave() {
  editorError.value = '';
  const specs: FieldValidationSpec[] = [
    { name: 'model', value: editorModel.value, rules: [{ kind: 'required' }] },
    {
      name: 'input',
      value: editorInput.value,
      rules: [{ kind: 'required' }, { kind: 'usd', min: 0 }],
    },
    {
      name: 'output',
      value: editorOutput.value,
      rules: [{ kind: 'required' }, { kind: 'usd', min: 0 }],
    },
    { name: 'cacheRead', value: editorCacheRead.value, rules: [{ kind: 'usd', min: 0 }] },
    { name: 'cacheWrite', value: editorCacheWrite.value, rules: [{ kind: 'usd', min: 0 }] },
  ];
  if (!validate(specs, t)) return;
  const inputMicros = parseUsdToMicros(editorInput.value);
  const outputMicros = parseUsdToMicros(editorOutput.value);
  if (inputMicros === null || outputMicros === null) {
    return;
  }
  saveMutation.mutate({
    model: editorModel.value.trim(),
    input_micros: inputMicros,
    output_micros: outputMicros,
    cache_read_micros: optionalMicros(editorCacheRead.value),
    cache_write_micros: optionalMicros(editorCacheWrite.value),
  });
}
</script>

<template>
  <FloatingWindow
    :title="initial === null ? t('pricing.editorCreate') : t('pricing.editorEdit')"
    :anchor="anchor"
    :stack-order="stackOrder"
    :cascade="cascade"
    :attention="attention"
    :topmost="topmost"
    @close="emit('close')"
    @pointerdown="emit('raise')"
  >
    <form novalidate @submit.prevent="handleSave">
      <div class="card-body space-y-3">
        <FormField
          field-name="model"
          :label="t('pricing.model')"
          :input-id="modelInputId"
          :error="fieldError('model')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="modelInputId"
              v-model="editorModel"
              type="text"
              :disabled="initial !== null"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('model')"
            />
          </template>
        </FormField>
        <FormField
          field-name="input"
          :label="t('pricing.input')"
          :input-id="inputInputId"
          :error="fieldError('input')"
          :guide="t('pricing.usdGuide')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="inputInputId"
              v-model="editorInput"
              type="text"
              inputmode="decimal"
              class="font-mono"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('input')"
            />
          </template>
        </FormField>
        <FormField
          field-name="output"
          :label="t('pricing.output')"
          :input-id="outputInputId"
          :error="fieldError('output')"
          :guide="t('pricing.usdGuide')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="outputInputId"
              v-model="editorOutput"
              type="text"
              inputmode="decimal"
              class="font-mono"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('output')"
            />
          </template>
        </FormField>
        <FormField
          field-name="cacheRead"
          :label="t('pricing.cacheRead')"
          :input-id="cacheReadInputId"
          :error="fieldError('cacheRead')"
          :guide="t('pricing.optionalUsdGuide')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="cacheReadInputId"
              v-model="editorCacheRead"
              type="text"
              inputmode="decimal"
              class="font-mono"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('cacheRead')"
            />
          </template>
        </FormField>
        <FormField
          field-name="cacheWrite"
          :label="t('pricing.cacheWrite')"
          :input-id="cacheWriteInputId"
          :error="fieldError('cacheWrite')"
          :guide="t('pricing.optionalUsdGuide')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="cacheWriteInputId"
              v-model="editorCacheWrite"
              type="text"
              inputmode="decimal"
              class="font-mono"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('cacheWrite')"
            />
          </template>
        </FormField>
        <p v-if="editorError" class="text-danger text-sm" data-testid="pricing-editor-error">
          {{ editorError }}
        </p>
      </div>
      <div class="card-footer card-body flex justify-end gap-2">
        <button type="button" class="btn" @click="emit('close')">
          {{ t('common.cancel') }}
        </button>
        <button
          type="submit"
          class="btn btn-primary"
          data-testid="pricing-save-entry"
          :disabled="saveMutation.isPending.value"
        >
          {{ t('common.save') }}
        </button>
      </div>
    </form>
  </FloatingWindow>
</template>
