<script setup lang="ts">
// 渠道编辑器浮窗：每个实例自持草稿，向窗口栈上报脏状态以供淘汰判定。
// 模型清单以 chip 呈现（点击复制、可删除、自然排序，最多露出 9 个、其余收进 +N）：
// 带别名的主模型与别名本体以别名色底区分，悬浮提示互指；删别名清空映射，删主
// 模型名保留别名（同步视图以「仅别名生效」呈现）。清单下方可手动输入模型 ID
// 追加进草稿；「设置模型」切换到 ChannelModelSync 表格视图，点「保存并返回」把
// 勾选模型与别名映射写回草稿，右上角关闭为「不保存并返回」；保存仍走表单页签
// 的既有流程。
import { useId, computed, ref, useTemplateRef, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from 'reka-ui';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import { channelWriteBody, type Channel, type ChannelView, type Protocol } from '@/api/types';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormPasswordInput from '@/components/ui/FormPasswordInput.vue';
import FormSwitch from '@/components/ui/FormSwitch.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import SegmentSwitch, { type SegmentPair } from '@/components/ui/SegmentSwitch.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { useToast } from '@/composables/useToast';
import ChannelEditorChip from '@/features/channel/ChannelEditorChip.vue';
import ChannelModelSync from '@/features/channel/ChannelModelSync.vue';
import { compareModels, sameAliasMap, sameModelSet } from '@/lib/model-list';
import { parseOptionalUint } from '@/lib/uint-parse';
import { DEFAULT_MODEL_GROUP, groupSelectOptions } from '@/lib/visible-models';
import type { FieldValidationSpec } from '@/lib/form-validation';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const PROTOCOLS: Protocol[] = ['openai_chat', 'openai_responses', 'anthropic_messages'];

type EditorTab = 'basic' | 'advanced';

/** 高级设置页签内的字段：保存校验失败时需切回该页签才能看到错误。 */
const ADVANCED_FIELDS: ReadonlySet<string> = new Set(['timeoutMs', 'maxRetries']);

/** 两列网格末格留给 +N；露出奇数个 chip，避免把编辑器撑高。 */
const EDITOR_MODEL_VISIBLE_COUNT = 9;

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
const { copy, error } = useToast();

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
const timeoutMsInputId = `channel-editor-timeout-ms-${uid}`;
const maxRetriesInputId = `channel-editor-max-retries-${uid}`;
const enabledInputId = `channel-editor-enabled-${uid}`;
const groupInputId = `channel-editor-group-${uid}`;
const addModelInputId = `channel-editor-add-model-${uid}`;

const queryClient = useQueryClient();
const { activeError, fieldError, fieldInputHandlers, showFieldError, dismissError, validate } =
  useFormValidation();

const protocolOptions = computed(() =>
  PROTOCOLS.map((value) => ({
    value,
    label: t(`protocol.${value}`),
  })),
);

const initialValues = {
  name: props.initial?.name ?? '',
  protocol: props.initial?.protocol ?? 'openai_chat',
  baseUrl: props.initial?.base_url ?? '',
  apiKey: props.initial?.api_key ?? '',
  models: props.initial ? [...props.initial.models] : ([] as string[]),
  aliases: props.initial ? { ...props.initial.model_aliases } : ({} as Record<string, string>),
  timeoutMs: String(props.initial?.timeout_ms ?? '30000'),
  maxRetries: String(props.initial?.max_retries ?? '0'),
  enabled: props.initial?.enabled ?? true,
  modelGroup: props.initial?.model_group ?? DEFAULT_MODEL_GROUP,
};

const editorName = ref(initialValues.name);
const editorProtocol = ref<Protocol>(initialValues.protocol);
const editorBaseUrl = ref(initialValues.baseUrl);
const editorApiKey = ref(initialValues.apiKey);
const editorModels = ref<string[]>(initialValues.models);
/** 别名映射草稿（别名 → 主模型名）：仅在同步表格中编辑。 */
const editorAliasesMap = ref<Record<string, string>>(initialValues.aliases);
const editorTimeoutMs = ref(initialValues.timeoutMs);
const editorMaxRetries = ref(initialValues.maxRetries);
const editorEnabled = ref(initialValues.enabled);
const editorGroup = ref(initialValues.modelGroup);
/** 手动添加模型 ID 的输入草稿；点添加后 trim 写入 `editorModels`。 */
const addModelDraft = ref('');

const dirty = computed(
  () =>
    editorName.value !== initialValues.name ||
    editorProtocol.value !== initialValues.protocol ||
    editorBaseUrl.value !== initialValues.baseUrl ||
    editorApiKey.value !== initialValues.apiKey ||
    !sameModelSet(editorModels.value, initialValues.models) ||
    !sameAliasMap(editorAliasesMap.value, initialValues.aliases) ||
    editorTimeoutMs.value !== initialValues.timeoutMs ||
    editorMaxRetries.value !== initialValues.maxRetries ||
    editorEnabled.value !== initialValues.enabled ||
    editorGroup.value !== initialValues.modelGroup,
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const groupsQuery = useQuery({
  queryKey: ['model-groups'],
  queryFn: () => apiClient.listModelGroups(),
});

const groupOptions = computed(() =>
  groupSelectOptions(groupsQuery.data.value ?? [], editorGroup.value, t('models.ungrouped')),
);

const saveMutation = useMutation({
  mutationFn: (body: Channel) =>
    props.initial === null
      ? apiClient.createChannel(body)
      : apiClient.updateChannel(props.initial.id, body),
  onSuccess: async () => {
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['channels'] });
    await queryClient.invalidateQueries({ queryKey: ['model-groups'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

// --- 模型清单 chip ---

/** 清单 chip：主模型名与别名混排；带别名关系者染别名底色并以 tooltip 互指。 */
interface ModelChip {
  name: string;
  isAlias: boolean;
  /** 别名本体与带别名的主模型共用别名色底；无别名关系的主模型用普通底。 */
  hasAliasRelation: boolean;
  /** tooltip 文案：主模型名列出其别名，别名列出其主模型名；空串表示无 tooltip。 */
  tooltip: string;
}

const modelChips = computed((): ModelChip[] => {
  const listed = new Set(editorModels.value);
  const canonicalAliases = new Map<string, string[]>();
  for (const [alias, canonical] of Object.entries(editorAliasesMap.value)) {
    const list = canonicalAliases.get(canonical);
    if (list) {
      list.push(alias);
    } else {
      canonicalAliases.set(canonical, [alias]);
    }
  }
  const chips: ModelChip[] = editorModels.value.map((name) => {
    const canonical = editorAliasesMap.value[name];
    if (canonical !== undefined) {
      return {
        name,
        isAlias: true,
        hasAliasRelation: true,
        tooltip: t('channel.chipAliasTooltip', { canonical }),
      };
    }
    const aliases = canonicalAliases.get(name) ?? [];
    const hasAliasRelation = aliases.length > 0;
    return {
      name,
      isAlias: false,
      hasAliasRelation,
      tooltip: hasAliasRelation
        ? t('channel.chipCanonicalTooltip', { aliases: aliases.join(', ') })
        : '',
    };
  });
  for (const [alias, canonical] of Object.entries(editorAliasesMap.value)) {
    if (listed.has(alias)) continue;
    chips.push({
      name: alias,
      isAlias: true,
      hasAliasRelation: true,
      tooltip: t('channel.chipAliasTooltip', { canonical }),
    });
  }
  chips.sort((left, right) => compareModels(left.name, right.name));
  return chips;
});

const visibleModelChips = computed(() => modelChips.value.slice(0, EDITOR_MODEL_VISIBLE_COUNT));
const hiddenModelChips = computed(() => modelChips.value.slice(EDITOR_MODEL_VISIBLE_COUNT));
const hiddenModelCount = computed(() => hiddenModelChips.value.length);

function removeChip(chip: ModelChip) {
  editorModels.value = editorModels.value.filter((item) => item !== chip.name);
  if (chip.isAlias) {
    // 删别名：同时清空其映射；别名 key 可能不在 `models` 里。
    editorAliasesMap.value = Object.fromEntries(
      Object.entries(editorAliasesMap.value).filter(([alias]) => alias !== chip.name),
    );
    return;
  }
  // 删主模型名：把指向它的昵称写入清单，同步视图呈「仅别名生效」。
  const nicknames = Object.entries(editorAliasesMap.value)
    .filter(([alias, canonical]) => canonical === chip.name && alias !== chip.name)
    .map(([alias]) => alias)
    .filter((alias) => !editorModels.value.includes(alias));
  if (nicknames.length > 0) {
    editorModels.value = [...editorModels.value, ...nicknames];
  }
}

/** 把 trim 后的 ID 追加进草稿清单。已是别名 key 的名字不可再当作独立主模型加入。 */
function addModel() {
  const id = addModelDraft.value.trim();
  if (id === '') {
    showFieldError('addModel', t('channel.addModelEmpty'));
    return;
  }
  if (Object.hasOwn(editorAliasesMap.value, id)) {
    showFieldError('addModel', t('channel.addModelIsAlias'));
    return;
  }
  if (editorModels.value.includes(id)) {
    showFieldError('addModel', t('channel.addModelDuplicate'));
    return;
  }
  dismissError();
  editorModels.value = [...editorModels.value, id];
  addModelDraft.value = '';
}

/** 有内容时才显示叉号；mousedown.prevent 避免抢焦点，与搜索框清除同一套交互。 */
function clearAddModelDraft() {
  addModelDraft.value = '';
}

// --- chip 点击复制模型名 ---

function copyModel(model: string) {
  void copy(model, t('common.copiedModelName'), t('common.copyFailedModelName'));
}

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

/** 同步视图保存并返回：勾选模型与别名映射写回草稿，恢复窗口自适应尺寸。 */
function handleSyncBack(models: string[], aliases: Record<string, string>) {
  editorModels.value = models;
  editorAliasesMap.value = aliases;
  leaveSyncView();
}

function leaveSyncView() {
  floatingWindow.value?.unlockSize();
  editorView.value = 'form';
}

/** 同步视图下关闭按钮/Esc 不关窗，只丢弃同步草稿并回到表单。 */
function handleWindowClose() {
  if (editorView.value === 'sync') {
    leaveSyncView();
    return;
  }
  emit('close');
}

// --- 保存 ---

function handleSave() {
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
  const timeoutMs = parseOptionalUint(editorTimeoutMs.value);
  const maxRetries = parseOptionalUint(editorMaxRetries.value);
  if (timeoutMs === null || maxRetries === null) {
    return;
  }
  const models = [...editorModels.value].sort(compareModels);
  const modelAliases = { ...editorAliasesMap.value };
  if (props.initial === null) {
    saveMutation.mutate({
      name: editorName.value.trim(),
      protocol: editorProtocol.value,
      base_url: editorBaseUrl.value.trim(),
      api_key: editorApiKey.value,
      models,
      model_aliases: modelAliases,
      priority: 0,
      weight: 1,
      timeout_ms: timeoutMs,
      max_retries: maxRetries,
      enabled: editorEnabled.value,
      model_group: editorGroup.value,
    });
    return;
  }
  // 编辑以列表中最新定义为基底整体替换写：开窗期间行内改过的字段
  // （如优先级/权重）不会被开窗时刻的旧快照覆盖。
  const latest = queryClient
    .getQueryData<ChannelView[]>(['channels'])
    ?.find((item) => item.id === props.initial?.id);
  if (!latest) {
    error(t('channel.goneOnSave'));
    return;
  }
  saveMutation.mutate({
    ...channelWriteBody(latest),
    name: editorName.value.trim(),
    protocol: editorProtocol.value,
    base_url: editorBaseUrl.value.trim(),
    api_key: editorApiKey.value,
    models,
    model_aliases: modelAliases,
    timeout_ms: timeoutMs,
    max_retries: maxRetries,
    enabled: editorEnabled.value,
    model_group: editorGroup.value,
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
    :close-aria-label="editorView === 'sync' ? t('channel.syncDiscard') : t('common.close')"
    @close="handleWindowClose"
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
        <!-- 两个页签叠放在同一网格单元：窗口高度取两者最大值，切换页签尺寸不变。
             隐藏一侧用 visibility 保留占位且不可聚焦、不可交互。 -->
        <div class="grid">
          <div
            class="col-start-1 row-start-1 space-y-3"
            :class="editorTab !== 'basic' && 'invisible'"
          >
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
                <UiSelect
                  :id="protocolInputId"
                  v-model="editorProtocol"
                  :options="protocolOptions"
                />
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
              field-name="modelGroup"
              :label="t('channel.modelGroup')"
              :input-id="groupInputId"
              :guide="t('channel.modelGroupGuide')"
            >
              <UiSelect
                :id="groupInputId"
                v-model="editorGroup"
                :options="groupOptions"
                data-testid="channel-editor-group"
              />
            </FormField>
            <fieldset class="border-seed rounded-md border p-3">
              <legend
                class="text-fg-muted flex w-full items-center gap-1.5 px-1 text-xs font-medium"
              >
                {{ t('channel.modelsTitle') }}
                <span
                  class="badge badge-neutral font-mono"
                  data-testid="channel-model-count"
                  :aria-label="t('channel.modelsTitle')"
                >
                  {{ modelChips.length }}
                </span>
                <span class="legend-rule" aria-hidden="true" />
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
              <ul v-if="modelChips.length > 0" class="grid grid-cols-2 gap-1.5">
                <li v-for="chip in visibleModelChips" :key="chip.name">
                  <ChannelEditorChip
                    :name="chip.name"
                    :tooltip="chip.tooltip"
                    :has-alias-relation="chip.hasAliasRelation"
                    @copy="copyModel(chip.name)"
                    @remove="removeChip(chip)"
                  />
                </li>
                <li v-if="hiddenModelCount > 0" class="flex items-center">
                  <PopoverRoot>
                    <PopoverTrigger as-child>
                      <button
                        type="button"
                        class="badge badge-neutral cursor-pointer"
                        data-testid="overflow-more"
                        :aria-label="t('common.moreCount', { count: hiddenModelCount })"
                      >
                        {{ t('common.moreCount', { count: hiddenModelCount }) }}
                      </button>
                    </PopoverTrigger>
                    <PopoverPortal>
                      <PopoverContent
                        align="start"
                        :side-offset="4"
                        class="data-table-menu overflow-chip-menu seed-scrollbar"
                        data-testid="overflow-chip-menu"
                      >
                        <ul class="overflow-chip-grid">
                          <li v-for="chip in hiddenModelChips" :key="chip.name" class="min-w-0">
                            <ChannelEditorChip
                              :name="chip.name"
                              :tooltip="chip.tooltip"
                              :has-alias-relation="chip.hasAliasRelation"
                              @copy="copyModel(chip.name)"
                              @remove="removeChip(chip)"
                            />
                          </li>
                        </ul>
                      </PopoverContent>
                    </PopoverPortal>
                  </PopoverRoot>
                </li>
              </ul>
              <p v-else class="text-fg-muted text-xs" data-testid="channel-models-empty">
                {{ t('channel.modelsEmpty') }}
              </p>
              <div class="relative mt-1.5" data-form-field="addModel">
                <div class="flex items-center gap-1.5">
                  <div class="relative min-w-0 flex-1">
                    <FormTextInput
                      :id="addModelInputId"
                      v-model="addModelDraft"
                      type="text"
                      class="input-with-clear font-mono text-xs"
                      :placeholder="t('channel.addModelPlaceholder')"
                      :invalid="Boolean(fieldError('addModel'))"
                      :hint-id="`${addModelInputId}-error`"
                      :aria-label="t('channel.addModelPlaceholder')"
                      data-testid="channel-add-model-input"
                      v-on="fieldInputHandlers('addModel')"
                      @keydown.enter.prevent="addModel"
                    />
                    <button
                      v-if="addModelDraft"
                      type="button"
                      class="search-input-clear top-1/2 -translate-y-1/2"
                      :aria-label="t('common.clearInput')"
                      data-testid="channel-add-model-clear"
                      @mousedown.prevent="clearAddModelDraft"
                    >
                      <UiIcon name="close" :size="12" />
                    </button>
                  </div>
                  <button
                    type="button"
                    class="btn shrink-0"
                    data-testid="channel-add-model"
                    @click="addModel"
                  >
                    {{ t('channel.addModel') }}
                  </button>
                </div>
                <p
                  v-if="fieldError('addModel')"
                  :id="`${addModelInputId}-error`"
                  class="form-field-hint"
                  role="alert"
                >
                  {{ fieldError('addModel') }}
                </p>
              </div>
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
          </div>
          <div
            class="col-start-1 row-start-1 space-y-3"
            :class="editorTab !== 'advanced' && 'invisible'"
          >
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
          </div>
        </div>
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
      :aliases="editorAliasesMap"
      :protocol="editorProtocol"
      :base-url="editorBaseUrl.trim()"
      :api-key="editorApiKey"
      :timeout-ms="syncTimeoutMs"
      :stack-order="stackOrder"
      @back="handleSyncBack"
      @cancel="leaveSyncView"
    />
  </FloatingWindow>
</template>
