<script setup lang="ts">
// 渠道编辑器浮窗：每个实例自持草稿，向窗口栈上报脏状态以供淘汰判定。
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import { channelWriteBody, type Channel, type ChannelView, type Protocol } from '@/api/types';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormPasswordInput from '@/components/ui/FormPasswordInput.vue';
import FormSwitch from '@/components/ui/FormSwitch.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import FormTextarea from '@/components/ui/FormTextarea.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { parseOptionalUint } from '@/lib/uint-parse';
import type { FieldValidationSpec } from '@/lib/form-validation';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const PROTOCOLS: Protocol[] = ['openai_chat', 'openai_responses', 'anthropic_messages'];

const props = withDefaults(
  defineProps<{
    /** 编辑对象；null 表示新建。 */
    initial: ChannelView | null;
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
const nameInputId = `channel-editor-name-${uid}`;
const protocolInputId = `channel-editor-protocol-${uid}`;
const baseUrlInputId = `channel-editor-base-url-${uid}`;
const apiKeyInputId = `channel-editor-api-key-${uid}`;
const modelsInputId = `channel-editor-models-${uid}`;
const aliasesInputId = `channel-editor-aliases-${uid}`;
const timeoutMsInputId = `channel-editor-timeout-ms-${uid}`;
const maxRetriesInputId = `channel-editor-max-retries-${uid}`;
const enabledInputId = `channel-editor-enabled-${uid}`;

const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();

const protocolOptions = computed(() =>
  PROTOCOLS.map((value) => ({
    value,
    label: t(`protocol.${value}`),
  })),
);

function parseModelList(text: string): string[] {
  return text
    .split(/[\n,]/)
    .map((item) => item.trim())
    .filter((item) => item.length > 0);
}

function formatModelList(models: string[]): string {
  return models.join('\n');
}

function parseAliases(text: string): Record<string, string> | null {
  const aliases: Record<string, string> = {};
  for (const line of text.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const eq = trimmed.indexOf('=');
    if (eq <= 0 || eq === trimmed.length - 1) {
      return null;
    }
    const alias = trimmed.slice(0, eq).trim();
    const canonical = trimmed.slice(eq + 1).trim();
    if (!alias || !canonical) {
      return null;
    }
    aliases[alias] = canonical;
  }
  return aliases;
}

function formatAliases(aliases: Record<string, string>): string {
  return Object.entries(aliases)
    .map(([alias, canonical]) => `${alias}=${canonical}`)
    .join('\n');
}

const initialValues = {
  name: props.initial?.name ?? '',
  protocol: props.initial?.protocol ?? 'openai_chat',
  baseUrl: props.initial?.base_url ?? '',
  apiKey: props.initial?.api_key ?? '',
  models: props.initial ? formatModelList(props.initial.models) : '',
  aliases: props.initial ? formatAliases(props.initial.model_aliases) : '',
  timeoutMs: String(props.initial?.timeout_ms ?? '30000'),
  maxRetries: String(props.initial?.max_retries ?? '0'),
  enabled: props.initial?.enabled ?? true,
};

const editorName = ref(initialValues.name);
const editorProtocol = ref<Protocol>(initialValues.protocol);
const editorBaseUrl = ref(initialValues.baseUrl);
const editorApiKey = ref(initialValues.apiKey);
const editorModels = ref(initialValues.models);
const editorAliases = ref(initialValues.aliases);
const editorTimeoutMs = ref(initialValues.timeoutMs);
const editorMaxRetries = ref(initialValues.maxRetries);
const editorEnabled = ref(initialValues.enabled);
const editorError = ref('');

const dirty = computed(
  () =>
    editorName.value !== initialValues.name ||
    editorProtocol.value !== initialValues.protocol ||
    editorBaseUrl.value !== initialValues.baseUrl ||
    editorApiKey.value !== initialValues.apiKey ||
    editorModels.value !== initialValues.models ||
    editorAliases.value !== initialValues.aliases ||
    editorTimeoutMs.value !== initialValues.timeoutMs ||
    editorMaxRetries.value !== initialValues.maxRetries ||
    editorEnabled.value !== initialValues.enabled,
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const saveMutation = useMutation({
  mutationFn: (body: Channel) =>
    props.initial === null
      ? apiClient.createChannel(body)
      : apiClient.updateChannel(props.initial.id, body),
  onSuccess: async () => {
    editorError.value = '';
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['channels'] });
  },
  onError: (err) => {
    editorError.value = extractApiError(err).message;
  },
});

function handleSave() {
  editorError.value = '';
  const specs: FieldValidationSpec[] = [
    { name: 'name', value: editorName.value, rules: [{ kind: 'required' }] },
    { name: 'baseUrl', value: editorBaseUrl.value, rules: [{ kind: 'required' }] },
    { name: 'apiKey', value: editorApiKey.value, rules: [{ kind: 'required' }] },
    {
      name: 'timeoutMs',
      value: editorTimeoutMs.value,
      rules: [{ kind: 'required' }, { kind: 'uint', min: 1 }],
    },
    {
      name: 'maxRetries',
      value: editorMaxRetries.value,
      rules: [{ kind: 'required' }, { kind: 'uint' }],
    },
  ];
  if (!validate(specs, t)) return;
  const aliases = parseAliases(editorAliases.value);
  if (aliases === null) {
    editorError.value = t('channel.aliasesGuide');
    return;
  }
  const timeoutMs = parseOptionalUint(editorTimeoutMs.value);
  const maxRetries = parseOptionalUint(editorMaxRetries.value);
  if (timeoutMs === null || maxRetries === null) {
    return;
  }
  if (props.initial === null) {
    saveMutation.mutate({
      name: editorName.value.trim(),
      protocol: editorProtocol.value,
      base_url: editorBaseUrl.value.trim(),
      api_key: editorApiKey.value,
      models: parseModelList(editorModels.value),
      model_aliases: aliases,
      priority: 0,
      weight: 1,
      timeout_ms: timeoutMs,
      max_retries: maxRetries,
      enabled: editorEnabled.value,
    });
    return;
  }
  // 编辑以列表中最新定义为基底整体替换写：开窗期间行内改过的字段
  // （如优先级/权重）不会被开窗时刻的旧快照覆盖。
  const latest = queryClient
    .getQueryData<ChannelView[]>(['channels'])
    ?.find((item) => item.id === props.initial?.id);
  if (!latest) {
    editorError.value = t('channel.goneOnSave');
    return;
  }
  saveMutation.mutate({
    ...channelWriteBody(latest),
    name: editorName.value.trim(),
    protocol: editorProtocol.value,
    base_url: editorBaseUrl.value.trim(),
    api_key: editorApiKey.value,
    models: parseModelList(editorModels.value),
    model_aliases: aliases,
    timeout_ms: timeoutMs,
    max_retries: maxRetries,
    enabled: editorEnabled.value,
  });
}
</script>

<template>
  <FloatingWindow
    wide
    :title="initial === null ? t('channel.editorCreate') : t('channel.editorEdit')"
    :anchor="anchor"
    :stack-order="stackOrder"
    :cascade="cascade"
    :attention="attention"
    :topmost="topmost"
    @close="emit('close')"
    @pointerdown="emit('raise')"
  >
    <form novalidate data-testid="channel-form" @submit.prevent="handleSave">
      <div class="card-body space-y-3">
        <FormField
          field-name="name"
          :label="t('channel.name')"
          :input-id="nameInputId"
          :error="fieldError('name')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="nameInputId"
              v-model="editorName"
              type="text"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('name')"
            />
          </template>
        </FormField>
        <FormField field-name="protocol" :label="t('channel.protocol')" :input-id="protocolInputId">
          <template #default>
            <UiSelect :id="protocolInputId" v-model="editorProtocol" :options="protocolOptions" />
          </template>
        </FormField>
        <FormField
          field-name="baseUrl"
          :label="t('channel.baseUrl')"
          :input-id="baseUrlInputId"
          :error="fieldError('baseUrl')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="baseUrlInputId"
              v-model="editorBaseUrl"
              type="url"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('baseUrl')"
            />
          </template>
        </FormField>
        <FormField
          field-name="apiKey"
          :label="t('channel.apiKey')"
          :input-id="apiKeyInputId"
          :error="fieldError('apiKey')"
        >
          <template #default="{ hintId, invalid }">
            <FormPasswordInput
              :id="apiKeyInputId"
              v-model="editorApiKey"
              autocomplete="off"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('apiKey')"
            />
          </template>
        </FormField>
        <FormField
          field-name="models"
          :label="t('channel.models')"
          :input-id="modelsInputId"
          :guide="t('channel.modelsGuide')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextarea
              :id="modelsInputId"
              v-model="editorModels"
              rows="3"
              :invalid="invalid"
              :hint-id="hintId"
            />
          </template>
        </FormField>
        <FormField
          field-name="aliases"
          :label="t('channel.aliases')"
          :input-id="aliasesInputId"
          :guide="t('channel.aliasesGuide')"
        >
          <template #default>
            <FormTextarea :id="aliasesInputId" v-model="editorAliases" rows="3" />
          </template>
        </FormField>
        <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <FormField
            field-name="timeoutMs"
            :label="t('channel.timeoutMs')"
            :input-id="timeoutMsInputId"
            :error="fieldError('timeoutMs')"
          >
            <template #default="{ hintId, invalid }">
              <FormTextInput
                :id="timeoutMsInputId"
                v-model="editorTimeoutMs"
                type="text"
                inputmode="numeric"
                :invalid="invalid"
                :hint-id="hintId"
                v-on="fieldInputHandlers('timeoutMs')"
              />
            </template>
          </FormField>
          <FormField
            field-name="maxRetries"
            :label="t('channel.maxRetries')"
            :input-id="maxRetriesInputId"
            :error="fieldError('maxRetries')"
          >
            <template #default="{ hintId, invalid }">
              <FormTextInput
                :id="maxRetriesInputId"
                v-model="editorMaxRetries"
                type="text"
                inputmode="numeric"
                :invalid="invalid"
                :hint-id="hintId"
                v-on="fieldInputHandlers('maxRetries')"
              />
            </template>
          </FormField>
        </div>
        <FormField
          field-name="enabled"
          layout="inline"
          :label="t('channel.enabled')"
          :input-id="enabledInputId"
          :guide="t('channel.enabledGuide')"
        >
          <FormSwitch
            :id="enabledInputId"
            v-model="editorEnabled"
            data-testid="channel-enabled-switch"
          />
        </FormField>
        <p v-if="editorError" class="text-danger text-sm" data-testid="channel-editor-error">
          {{ editorError }}
        </p>
      </div>
      <div class="card-footer card-body flex justify-between gap-2">
        <button type="button" class="btn" @click="emit('close')">
          {{ t('common.cancel') }}
        </button>
        <button
          type="submit"
          class="btn btn-primary"
          data-testid="channel-save"
          :disabled="saveMutation.isPending.value"
        >
          {{ t('common.save') }}
        </button>
      </div>
    </form>
  </FloatingWindow>
</template>
