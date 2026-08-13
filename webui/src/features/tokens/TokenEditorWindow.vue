<script setup lang="ts">
// 令牌编辑器浮窗：每个实例自持草稿，向窗口栈上报脏状态以供淘汰判定。
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { Token } from '@/api/types';
import type { TokenRow } from '@/api/token-rows';
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
    initial: TokenRow | null;
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
const keyInputId = `token-editor-key-${uid}`;
const nameInputId = `token-editor-name-${uid}`;
const limitInputId = `token-editor-limit-${uid}`;

const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();

const initialKey = props.initial ? props.initial.token_key : '';
const initialName = props.initial ? props.initial.name : '';
const initialLimit =
  props.initial && props.initial.limit_usd_micros !== null
    ? formatUsdAmount(props.initial.limit_usd_micros)
    : '';

const editorKey = ref(initialKey);
const editorName = ref(initialName);
const editorLimit = ref(initialLimit);
const editorError = ref('');

const dirty = computed(
  () =>
    editorKey.value !== initialKey ||
    editorName.value !== initialName ||
    editorLimit.value !== initialLimit,
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const saveMutation = useMutation({
  mutationFn: (body: Token) =>
    props.initial === null ? apiClient.createToken(body) : apiClient.updateToken(initialKey, body),
  onSuccess: async () => {
    editorError.value = '';
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['tokens'] });
  },
  onError: (err) => {
    editorError.value = extractApiError(err).message;
  },
});

function handleSave() {
  editorError.value = '';
  const specs: FieldValidationSpec[] = [
    { name: 'tokenKey', value: editorKey.value, rules: [{ kind: 'required' }] },
    { name: 'name', value: editorName.value, rules: [{ kind: 'required' }] },
    { name: 'limit', value: editorLimit.value, rules: [{ kind: 'usd', min: 0 }] },
  ];
  if (!validate(specs, t)) return;
  const limit = parseUsdToMicros(editorLimit.value);
  saveMutation.mutate({
    token_key: editorKey.value.trim(),
    name: editorName.value.trim(),
    limit_usd_micros: limit,
  });
}
</script>

<template>
  <FloatingWindow
    :title="initial === null ? t('tokens.editorCreate') : t('tokens.editorEdit')"
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
          field-name="tokenKey"
          :label="t('tokens.key')"
          :input-id="keyInputId"
          :error="fieldError('tokenKey')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="keyInputId"
              v-model="editorKey"
              type="text"
              class="font-mono"
              :disabled="initial !== null"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('tokenKey')"
            />
          </template>
        </FormField>
        <FormField
          field-name="name"
          :label="t('tokens.name')"
          :input-id="nameInputId"
          :error="fieldError('name')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="nameInputId"
              v-model="editorName"
              type="text"
              :placeholder="t('tokens.namePlaceholder')"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('name')"
            />
          </template>
        </FormField>
        <FormField
          field-name="limit"
          :label="t('tokens.limit')"
          :input-id="limitInputId"
          :error="fieldError('limit')"
          :guide="t('tokens.limitGuide')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="limitInputId"
              v-model="editorLimit"
              type="text"
              inputmode="decimal"
              class="font-mono"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('limit')"
            />
          </template>
        </FormField>
        <p v-if="editorError" class="text-danger text-sm" data-testid="token-editor-error">
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
          data-testid="token-save"
          :disabled="saveMutation.isPending.value"
        >
          {{ t('common.save') }}
        </button>
      </div>
    </form>
  </FloatingWindow>
</template>
