<script setup lang="ts">
// 渠道编辑器浮窗：每个实例自持草稿，向窗口栈上报脏状态以供淘汰判定。
// 模型清单以 chip 呈现（点击复制、可删除、自然排序）；「同步上游模型列表」
// 切换到 ChannelModelSync 选择表格视图，点「返回」把勾选结果写回草稿，
// 保存仍走表单页签的既有流程。
import { useId, computed, onUnmounted, ref, useTemplateRef, watch } from 'vue';
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
import SegmentSwitch, { type SegmentPair } from '@/components/ui/SegmentSwitch.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import ChannelModelSync from '@/features/channel/ChannelModelSync.vue';
import { compareModels, sameModelSet } from '@/lib/model-list';
import { parseOptionalUint } from '@/lib/uint-parse';
import type { FieldValidationSpec } from '@/lib/form-validation';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const PROTOCOLS: Protocol[] = ['openai_chat', 'openai_responses', 'anthropic_messages'];

type EditorTab = 'basic' | 'advanced';

/** 高级设置页签内的字段：保存校验失败时需切回该页签才能看到错误。 */
const ADVANCED_FIELDS: ReadonlySet<string> = new Set(['aliases', 'timeoutMs', 'maxRetries']);

/** chip 点击复制后「已复制」图标的展示时长。 */
const COPIED_HINT_MS = 1_500;

/** 编辑器两个视图：表单（含页签）与上游模型同步表格。 */
type EditorView = 'form' | 'sync';

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

const editorTitle = computed(() =>
  props.initial === null ? t('channel.editorCreate') : t('channel.editorEdit'),
);

const editorTab = ref<EditorTab>('basic');
const editorTabOptions = computed((): SegmentPair<EditorTab> => [
  { value: 'basic', label: t('channel.tabBasic'), testId: 'channel-editor-tab-basic' },
  { value: 'advanced', label: t('channel.tabAdvanced'), testId: 'channel-editor-tab-advanced' },
]);

const uid = useId();
const nameInputId = `channel-editor-name-${uid}`;
const protocolInputId = `channel-editor-protocol-${uid}`;
const baseUrlInputId = `channel-editor-base-url-${uid}`;
const apiKeyInputId = `channel-editor-api-key-${uid}`;
const aliasesInputId = `channel-editor-aliases-${uid}`;
const timeoutMsInputId = `channel-editor-timeout-ms-${uid}`;
const maxRetriesInputId = `channel-editor-max-retries-${uid}`;
const enabledInputId = `channel-editor-enabled-${uid}`;

const queryClient = useQueryClient();
const { activeError, fieldError, fieldInputHandlers, validate } = useFormValidation();

const protocolOptions = computed(() =>
  PROTOCOLS.map((value) => ({
    value,
    label: t(`protocol.${value}`),
  })),
);

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
  models: props.initial ? [...props.initial.models] : ([] as string[]),
  aliases: props.initial ? formatAliases(props.initial.model_aliases) : '',
  timeoutMs: String(props.initial?.timeout_ms ?? '30000'),
  maxRetries: String(props.initial?.max_retries ?? '0'),
  enabled: props.initial?.enabled ?? true,
};

const editorName = ref(initialValues.name);
const editorProtocol = ref<Protocol>(initialValues.protocol);
const editorBaseUrl = ref(initialValues.baseUrl);
const editorApiKey = ref(initialValues.apiKey);
const editorModels = ref<string[]>(initialValues.models);
const editorAliases = ref(initialValues.aliases);
const editorTimeoutMs = ref(initialValues.timeoutMs);
const editorMaxRetries = ref(initialValues.maxRetries);
const editorEnabled = ref(initialValues.enabled);
const editorError = ref('');

const sortedModels = computed(() => [...editorModels.value].sort(compareModels));

const dirty = computed(
  () =>
    editorName.value !== initialValues.name ||
    editorProtocol.value !== initialValues.protocol ||
    editorBaseUrl.value !== initialValues.baseUrl ||
    editorApiKey.value !== initialValues.apiKey ||
    !sameModelSet(editorModels.value, initialValues.models) ||
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

// --- 上游模型同步视图 ---

/** FloatingWindow 经 defineExpose 暴露的尺寸锁定能力。 */
interface FloatingWindowControls {
  lockSize: () => void;
  unlockSize: () => void;
}

const floatingWindow = useTemplateRef<FloatingWindowControls>('floatingWindow');
const editorView = ref<EditorView>('form');

/** 出站三要素缺一即无法拉取上游模型。 */
const canSync = computed(
  () => editorBaseUrl.value.trim() !== '' && editorApiKey.value.trim() !== '',
);

/** 草稿超时解析结果；非法传 null，由同步视图兜底缺省值。 */
const syncTimeoutMs = computed(() => parseOptionalUint(editorTimeoutMs.value));

function openSync() {
  // 锁定进入前的窗口尺寸：同步表格与表单内容高度不同，避免切换时窗口跳变。
  floatingWindow.value?.lockSize();
  editorView.value = 'sync';
}

/** 同步视图返回：勾选结果写回模型清单草稿，恢复窗口自适应尺寸。 */
function handleSyncBack(models: string[]) {
  editorModels.value = models;
  floatingWindow.value?.unlockSize();
  editorView.value = 'form';
}

function removeModel(model: string) {
  editorModels.value = editorModels.value.filter((item) => item !== model);
}

// --- chip 点击复制模型名 ---

const copiedModel = ref<string | null>(null);
let copiedTimer: ReturnType<typeof setTimeout> | undefined;

async function copyModel(model: string) {
  try {
    await navigator.clipboard.writeText(model);
  } catch {
    return;
  }
  copiedModel.value = model;
  if (copiedTimer !== undefined) clearTimeout(copiedTimer);
  copiedTimer = setTimeout(() => {
    if (copiedModel.value === model) copiedModel.value = null;
  }, COPIED_HINT_MS);
}

/** chip 键盘复制：仅 chip 本体聚焦时响应，内部删除按钮的按键不拦截。 */
function chipKeydown(event: KeyboardEvent, model: string) {
  if (event.target !== event.currentTarget) return;
  event.preventDefault();
  void copyModel(model);
}

onUnmounted(() => {
  if (copiedTimer !== undefined) clearTimeout(copiedTimer);
});

// --- 保存 ---

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
  if (!validate(specs, t)) {
    const failedField = activeError.value?.field;
    if (failedField && ADVANCED_FIELDS.has(failedField)) {
      editorTab.value = 'advanced';
    }
    return;
  }
  const aliases = parseAliases(editorAliases.value);
  if (aliases === null) {
    editorError.value = t('channel.aliasesGuide');
    editorTab.value = 'advanced';
    return;
  }
  const timeoutMs = parseOptionalUint(editorTimeoutMs.value);
  const maxRetries = parseOptionalUint(editorMaxRetries.value);
  if (timeoutMs === null || maxRetries === null) {
    return;
  }
  const models = [...editorModels.value].sort(compareModels);
  if (props.initial === null) {
    saveMutation.mutate({
      name: editorName.value.trim(),
      protocol: editorProtocol.value,
      base_url: editorBaseUrl.value.trim(),
      api_key: editorApiKey.value,
      models,
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
    models,
    model_aliases: aliases,
    timeout_ms: timeoutMs,
    max_retries: maxRetries,
    enabled: editorEnabled.value,
  });
}
</script>

<template>
  <FloatingWindow
    ref="floatingWindow"
    wide
    :title="editorTitle"
    :anchor="anchor"
    :stack-order="stackOrder"
    :cascade="cascade"
    :attention="attention"
    :topmost="topmost"
    @close="emit('close')"
    @pointerdown="emit('raise')"
  >
    <template #header-extra>
      <SegmentSwitch
        v-if="editorView === 'form'"
        v-model="editorTab"
        :options="editorTabOptions"
        :aria-label="editorTitle"
      />
    </template>

    <form
      v-if="editorView === 'form'"
      novalidate
      data-testid="channel-form"
      @submit.prevent="handleSave"
    >
      <div class="card-body space-y-3">
        <template v-if="editorTab === 'basic'">
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
          <FormField
            field-name="protocol"
            :label="t('channel.requestProtocol')"
            :input-id="protocolInputId"
          >
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
          <fieldset class="border-seed rounded-md border p-3">
            <legend class="text-fg-muted flex items-center gap-1.5 px-1 text-xs font-medium">
              {{ t('channel.modelsTitle') }}
              <span
                class="badge badge-neutral font-mono"
                data-testid="channel-model-count"
                :aria-label="t('channel.modelsTitle')"
              >
                {{ editorModels.length }}
              </span>
              <button
                type="button"
                class="legend-btn"
                data-testid="channel-sync-models"
                :disabled="!canSync"
                @click="openSync"
              >
                {{ t('channel.syncModels') }}
              </button>
            </legend>
            <ul v-if="sortedModels.length > 0" class="grid grid-cols-2 gap-1.5">
              <li
                v-for="model in sortedModels"
                :key="model"
                role="button"
                tabindex="0"
                class="flex cursor-pointer items-center gap-1 rounded-md bg-[var(--seed-surface-alt)] py-1 pr-1 pl-2"
                data-testid="channel-model-chip"
                :data-model="model"
                @click="copyModel(model)"
                @keydown.enter="chipKeydown($event, model)"
                @keydown.space="chipKeydown($event, model)"
              >
                <span class="min-w-0 flex-1 truncate font-mono text-xs">{{ model }}</span>
                <button
                  type="button"
                  class="text-fg-subtle hover:text-danger cursor-pointer rounded p-0.5 hover:bg-[var(--danger-bg)]"
                  data-testid="channel-model-remove"
                  :aria-label="t('channel.removeModel', { model })"
                  @click.stop="removeModel(model)"
                >
                  <UiIcon
                    :name="copiedModel === model ? 'check' : 'close'"
                    :size="12"
                    :class="copiedModel === model && 'text-success'"
                  />
                </button>
              </li>
            </ul>
            <p v-else class="text-fg-muted text-xs" data-testid="channel-models-empty">
              {{ t('channel.modelsEmpty') }}
            </p>
          </fieldset>
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
        </template>
        <template v-else>
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
        </template>
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

    <ChannelModelSync
      v-else
      :models="editorModels"
      :protocol="editorProtocol"
      :base-url="editorBaseUrl.trim()"
      :api-key="editorApiKey"
      :timeout-ms="syncTimeoutMs"
      :stack-order="stackOrder"
      @back="handleSyncBack"
    />
  </FloatingWindow>
</template>
