<script setup lang="ts">
// 定价编辑器：模型名由清单行带入，禁止手打新建价格行。
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { Price } from '@/api/types';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { useToast } from '@/composables/useToast';
import { formatUsdAmount, parseUsdToMicros } from '@/lib/format';
import type { FieldValidationSpec } from '@/lib/form-validation';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const props = withDefaults(
  defineProps<{
    /** 可调用名，始终锁定。 */
    model: string;
    /** 价格所属渠道，始终锁定。 */
    channelId: number;
    channelName: string;
    /** 已有价格；null 表示未定价、本次为创建。 */
    initial: Price | null;
    anchor?: FloatingWindowAnchor | null;
    stackOrder?: number;
    cascade?: number;
    attention?: boolean;
    topmost?: boolean;
    canFillFromCatalog?: boolean;
  }>(),
  {
    anchor: null,
    stackOrder: 0,
    cascade: 0,
    attention: false,
    topmost: true,
    canFillFromCatalog: false,
  },
);

const emit = defineEmits<{
  close: [];
  raise: [];
  'dirty-change': [dirty: boolean];
  /** 打开与清单多选共用的价格同步浮窗，并把当前行加入勾选。 */
  'catalog-sync': [];
}>();

const { t } = useI18n();
const { error } = useToast();

const uid = useId();
const modelInputId = `pricing-editor-model-${uid}`;
const channelInputId = `pricing-editor-channel-${uid}`;
const inputInputId = `pricing-editor-input-${uid}`;
const outputInputId = `pricing-editor-output-${uid}`;
const cacheReadInputId = `pricing-editor-cache-read-${uid}`;
const cacheWriteInputId = `pricing-editor-cache-write-${uid}`;
const cacheWrite1hInputId = `pricing-editor-cache-write-1h-${uid}`;

const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();

function formatOptional(micros: number | null): string {
  return micros === null ? '' : formatUsdAmount(micros);
}

const initialValues = {
  input: props.initial ? formatUsdAmount(props.initial.input_micros) : '',
  output: props.initial ? formatUsdAmount(props.initial.output_micros) : '',
  cacheRead: props.initial ? formatOptional(props.initial.cache_read_micros) : '',
  cacheWrite: props.initial ? formatOptional(props.initial.cache_write_micros) : '',
  cacheWrite1h: props.initial ? formatOptional(props.initial.cache_write_1h_micros) : '',
};

const editorInput = ref(initialValues.input);
const editorOutput = ref(initialValues.output);
const editorCacheRead = ref(initialValues.cacheRead);
const editorCacheWrite = ref(initialValues.cacheWrite);
const editorCacheWrite1h = ref(initialValues.cacheWrite1h);

const dirty = computed(
  () =>
    editorInput.value !== initialValues.input ||
    editorOutput.value !== initialValues.output ||
    editorCacheRead.value !== initialValues.cacheRead ||
    editorCacheWrite.value !== initialValues.cacheWrite ||
    editorCacheWrite1h.value !== initialValues.cacheWrite1h,
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const saveMutation = useMutation({
  mutationFn: (body: Price) =>
    props.initial === null
      ? apiClient.createPrice(body)
      : apiClient.updatePrice(props.channelId, props.model, body),
  onSuccess: async () => {
    emit('dirty-change', false);
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['prices'] });
    await queryClient.invalidateQueries({ queryKey: ['unified-models'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

function optionalMicros(value: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  return parseUsdToMicros(trimmed);
}

function handleSave() {
  const specs: FieldValidationSpec[] = [
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
    { name: 'cacheWrite1h', value: editorCacheWrite1h.value, rules: [{ kind: 'usd', min: 0 }] },
  ];
  if (!validate(specs, t)) return;
  const inputMicros = parseUsdToMicros(editorInput.value);
  const outputMicros = parseUsdToMicros(editorOutput.value);
  if (inputMicros === null || outputMicros === null) {
    return;
  }
  saveMutation.mutate({
    channel_id: props.channelId,
    model: props.model,
    input_micros: inputMicros,
    output_micros: outputMicros,
    cache_read_micros: optionalMicros(editorCacheRead.value),
    cache_write_micros: optionalMicros(editorCacheWrite.value),
    cache_write_1h_micros: optionalMicros(editorCacheWrite1h.value),
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
    <template #header-extra>
      <button
        v-if="canFillFromCatalog"
        type="button"
        class="btn btn-sm h-6 py-0 text-xs"
        data-testid="pricing-fill-from-catalog"
        @click="emit('catalog-sync')"
      >
        {{ t('models.catalogFillFromDir') }}
      </button>
    </template>
    <form novalidate @submit.prevent="handleSave">
      <div class="card-body space-y-3">
        <FormField field-name="channel" :label="t('pricing.channel')" :input-id="channelInputId">
          <template #default>
            <FormTextInput
              :id="channelInputId"
              :model-value="channelName"
              type="text"
              disabled
              data-testid="pricing-editor-channel"
            />
          </template>
        </FormField>
        <FormField field-name="model" :label="t('pricing.model')" :input-id="modelInputId">
          <template #default>
            <FormTextInput :id="modelInputId" :model-value="model" type="text" disabled />
          </template>
        </FormField>
        <div class="settings-fields-row">
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
        </div>
        <div class="settings-fields-row">
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
          <FormField
            field-name="cacheWrite1h"
            :label="t('pricing.cacheWrite1h')"
            :input-id="cacheWrite1hInputId"
            :error="fieldError('cacheWrite1h')"
            :guide="t('pricing.cacheWrite1hGuide')"
          >
            <template #default="{ hintId, invalid }">
              <FormTextInput
                :id="cacheWrite1hInputId"
                v-model="editorCacheWrite1h"
                type="text"
                inputmode="decimal"
                class="font-mono"
                :invalid="invalid"
                :hint-id="hintId"
                v-on="fieldInputHandlers('cacheWrite1h')"
              />
            </template>
          </FormField>
        </div>
      </div>
      <div class="card-footer card-body flex justify-between gap-2">
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
