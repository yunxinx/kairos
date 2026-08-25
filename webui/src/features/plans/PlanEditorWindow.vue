<script setup lang="ts">
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type {
  PlanAudience,
  PlanCapabilities,
  PlanCreate,
  PlanUpdate,
  PlanView,
} from '@/api/types';
import Checkbox from '@/components/ui/Checkbox.vue';
import DataTablePanel from '@/components/ui/DataTablePanel.vue';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormSwitch from '@/components/ui/FormSwitch.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import VirtualTable from '@/components/ui/table/VirtualTable.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { useToast } from '@/composables/useToast';
import { EMPTY_CAPABILITIES, PLAN_CAPABILITY_KEYS } from '@/lib/capabilities';
import { formatUsdAmount, parseUsdToMicros } from '@/lib/format';
import { groupDisplayName } from '@/lib/visible-models';
import type { FieldValidationSpec } from '@/lib/form-validation';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const props = withDefaults(
  defineProps<{
    initial: PlanView | null;
    /**
     * 新建时的受众；编辑时以 `initial.audience` 为准（受众建后不可改）。
     *
     * 受众决定这一档有没有管理面能力：用户档把能力开关整块藏掉，避免运营在
     * 「给用户的档」上误开 manage_users 之类的开关。
     */
    audience?: PlanAudience;
    anchor?: FloatingWindowAnchor | null;
    stackOrder?: number;
    cascade?: number;
    attention?: boolean;
    topmost?: boolean;
  }>(),
  {
    audience: 'user',
    anchor: null,
    stackOrder: 0,
    cascade: 0,
    attention: false,
    topmost: true,
  },
);

const emit = defineEmits<{
  close: [];
  raise: [];
  'dirty-change': [dirty: boolean];
}>();

const { t } = useI18n();
const { error } = useToast();
const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();

const uid = useId();
const internalId = `plan-internal-${uid}`;
const displayId = `plan-display-${uid}`;
const noteId = `plan-note-${uid}`;
const discountId = `plan-discount-${uid}`;
const defaultRpmId = `plan-default-rpm-${uid}`;
const sharedRpmId = `plan-shared-rpm-${uid}`;
const grantId = `plan-grant-${uid}`;

const isEdit = computed(() => props.initial !== null);

const initialInternal = props.initial?.internal_name ?? '';
const initialDisplay = props.initial?.display_name ?? '';
const initialNote = props.initial?.note ?? '';
const initialDiscount = props.initial ? String(props.initial.discount_bp / 100) : '100';
const initialDefaultRpm =
  props.initial?.default_rpm != null ? String(props.initial.default_rpm) : '';
const initialSharedRpm = props.initial?.shared_rpm != null ? String(props.initial.shared_rpm) : '';
const initialGrant = props.initial ? formatUsdAmount(props.initial.initial_grant_usd_micros) : '0';
const initialNoteVisible = props.initial?.note_visible_to_admin ?? false;
const initialSharedWithAdmin = props.initial?.shared_with_admin ?? false;
const initialCapabilities = props.initial
  ? { ...props.initial.capabilities }
  : { ...EMPTY_CAPABILITIES };
const initialGroups = props.initial ? [...props.initial.groups] : [];

/** 受众：编辑时锁定为已存档的值，新建时取调用方按钮传入的受众。 */
const audience = computed<PlanAudience>(() => props.initial?.audience ?? props.audience);
/** 用户档没有管理面，能力开关整块不渲染。 */
const showCapabilities = computed(() => audience.value === 'admin');

/**
 * 标题带上受众：两个「新建」入口打开的是同一个窗，只写「新建套餐」的话运营
 * 无法确认自己点的是哪一个。
 */
const windowTitle = computed(() => {
  const kind = audience.value === 'admin' ? t('plans.audienceAdmin') : t('plans.audienceUser');
  return isEdit.value
    ? t('plans.editorEditOf', { audience: kind })
    : t('plans.editorCreateOf', { audience: kind });
});

const internalName = ref(initialInternal);
const displayName = ref(initialDisplay);
const note = ref(initialNote);
const discountPercent = ref(initialDiscount);
const defaultRpm = ref(initialDefaultRpm);
const sharedRpm = ref(initialSharedRpm);
const grantUsd = ref(initialGrant);
const noteVisible = ref(initialNoteVisible);
const sharedWithAdmin = ref(initialSharedWithAdmin);
const capabilities = ref<PlanCapabilities>({ ...initialCapabilities });
const selectedGroups = ref<string[]>([...initialGroups]);
/** 仅创建时可请求“创建并设为默认”；编辑不承载默认身份。 */
const isDefault = ref(false);
const groupSearch = ref('');

const groupsQuery = useQuery({
  queryKey: ['model-groups'],
  queryFn: () => apiClient.listModelGroups(),
});

const allGroups = computed(() =>
  (groupsQuery.data.value ?? []).map((group) => group.name).sort((a, b) => a.localeCompare(b)),
);

/**
 * 组表一行：名字 + 是否已勾选。顺序恒为字典序，勾选不重排。
 *
 * 刻意不把已勾选的排到前面：那样每次点复选框，行都会从光标下跳走，连点几个组就
 * 会勾错。当前名单规模用标题旁的计数交代，不靠排序表达。
 */
const groupRows = computed(() => {
  const q = groupSearch.value.trim().toLowerCase();
  const selected = new Set(selectedGroups.value);
  return allGroups.value
    .filter((name) => q === '' || name.toLowerCase().includes(q))
    .map((name) => ({ name, checked: selected.has(name) }));
});

/** 与 `colspan` 等长；`table-layout:fixed` 下由 colgroup 定宽。 */
const groupColumns = [{ width: 'w-10' }, { width: 'auto' }];

function toggleGroup(name: string, checked: boolean) {
  if (checked) {
    if (!selectedGroups.value.includes(name)) {
      selectedGroups.value = [...selectedGroups.value, name];
    }
  } else {
    selectedGroups.value = selectedGroups.value.filter((item) => item !== name);
  }
}

const dirty = computed(() => {
  if (!isEdit.value) {
    return Boolean(
      internalName.value.trim() ||
      displayName.value.trim() ||
      note.value.trim() ||
      discountPercent.value.trim() ||
      defaultRpm.value.trim() ||
      sharedRpm.value.trim() ||
      grantUsd.value.trim() ||
      sharedWithAdmin.value ||
      isDefault.value ||
      selectedGroups.value.length > 0 ||
      PLAN_CAPABILITY_KEYS.some((key) => capabilities.value[key]),
    );
  }
  return (
    internalName.value !== initialInternal ||
    displayName.value !== initialDisplay ||
    note.value !== initialNote ||
    discountPercent.value !== initialDiscount ||
    defaultRpm.value !== initialDefaultRpm ||
    sharedRpm.value !== initialSharedRpm ||
    grantUsd.value !== initialGrant ||
    noteVisible.value !== initialNoteVisible ||
    sharedWithAdmin.value !== initialSharedWithAdmin ||
    PLAN_CAPABILITY_KEYS.some((key) => capabilities.value[key] !== initialCapabilities[key]) ||
    JSON.stringify([...selectedGroups.value].sort()) !== JSON.stringify([...initialGroups].sort())
  );
});
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

type SavePayload =
  | { kind: 'create'; body: PlanCreate }
  | { kind: 'update'; id: number; body: PlanUpdate };

const saveMutation = useMutation({
  mutationFn: (payload: SavePayload) =>
    payload.kind === 'create'
      ? apiClient.createPlan(payload.body)
      : apiClient.updatePlan(payload.id, payload.body),
  onSuccess: async () => {
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['plans'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

function handleSave() {
  const specs: FieldValidationSpec[] = [
    { name: 'internalName', value: internalName.value, rules: [{ kind: 'required' }] },
    { name: 'displayName', value: displayName.value, rules: [{ kind: 'required' }] },
    { name: 'discountPercent', value: discountPercent.value, rules: [{ kind: 'required' }] },
    { name: 'grantUsd', value: grantUsd.value, rules: [{ kind: 'usd' }] },
  ];
  if (defaultRpm.value.trim()) {
    specs.push({ name: 'defaultRpm', value: defaultRpm.value, rules: [{ kind: 'uint', min: 0 }] });
  }
  if (sharedRpm.value.trim()) {
    specs.push({ name: 'sharedRpm', value: sharedRpm.value, rules: [{ kind: 'uint', min: 0 }] });
  }
  if (!validate(specs, t)) return;
  const grantParsed = parseUsdToMicros(grantUsd.value);
  if (grantParsed === null || grantParsed < 0) {
    // validate 已覆盖格式；这里防御非负数。
    return;
  }
  const discountInput = discountPercent.value.trim();
  const discountRaw = discountInput.replace(/%$/, '');
  const discountNumber = Number(discountRaw);
  if (!Number.isFinite(discountNumber) || discountNumber < 0) return;
  let discountBp: number;
  if (discountInput.endsWith('%')) {
    discountBp = Math.round(discountNumber * 100);
  } else if (discountNumber < 10 && discountRaw.includes('.')) {
    // 支持规格中的倍率写法：0.8 → 8000 bp，1.2 → 12000 bp。
    discountBp = Math.round(discountNumber * 10_000);
  } else if (discountNumber === 1) {
    // 1 按原价倍率理解。
    discountBp = 10_000;
  } else {
    discountBp = Math.round(discountNumber * 100);
  }
  const body: PlanUpdate = {
    internal_name: internalName.value.trim(),
    display_name: displayName.value.trim(),
    note: note.value,
    note_visible_to_admin: noteVisible.value,
    discount_bp: discountBp,
    default_rpm: defaultRpm.value.trim() === '' ? null : Number(defaultRpm.value),
    shared_rpm: sharedRpm.value.trim() === '' ? null : Number(sharedRpm.value),
    initial_grant_usd_micros: grantParsed,
    // 用户档一律送全关：开关在界面上藏着，但编辑一档旧数据时 `capabilities` 里可能
    // 还留着历史真值，原样回送就等于悄悄保留了管理能力。
    capabilities: showCapabilities.value ? { ...capabilities.value } : { ...EMPTY_CAPABILITIES },
    shared_with_admin: sharedWithAdmin.value,
    groups: [...selectedGroups.value].sort(),
  };
  if (props.initial) {
    saveMutation.mutate({ kind: 'update', id: props.initial.id, body });
  } else {
    saveMutation.mutate({
      kind: 'create',
      body: { ...body, audience: audience.value, is_default: isDefault.value },
    });
  }
}

function capabilityLabel(key: keyof PlanCapabilities): string {
  return t(`plans.capabilities.${key}`);
}
</script>

<template>
  <FloatingWindow
    :title="windowTitle"
    :anchor="anchor"
    :stack-order="stackOrder"
    :cascade="cascade"
    :attention="attention"
    :topmost="topmost"
    wide
    @close="emit('close')"
    @pointerdown="emit('raise')"
  >
    <form novalidate @submit.prevent="handleSave">
      <div class="card-body space-y-4">
        <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <FormField
            field-name="internalName"
            :label="t('plans.internalName')"
            :input-id="internalId"
            :error="fieldError('internalName')"
          >
            <template #default="{ hintId, invalid }">
              <FormTextInput
                :id="internalId"
                v-model="internalName"
                type="text"
                data-testid="plan-internal-name"
                :invalid="invalid"
                :hint-id="hintId"
                v-on="fieldInputHandlers('internalName')"
              />
            </template>
          </FormField>
          <FormField
            field-name="displayName"
            :label="t('plans.displayName')"
            :input-id="displayId"
            :error="fieldError('displayName')"
          >
            <template #default="{ hintId, invalid }">
              <FormTextInput
                :id="displayId"
                v-model="displayName"
                type="text"
                data-testid="plan-display-name"
                :invalid="invalid"
                :hint-id="hintId"
                v-on="fieldInputHandlers('displayName')"
              />
            </template>
          </FormField>
        </div>

        <FormField
          field-name="discountPercent"
          :label="t('plans.discountPercent')"
          :input-id="discountId"
          :error="fieldError('discountPercent')"
          :guide="t('plans.discountGuide')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="discountId"
              v-model="discountPercent"
              type="text"
              inputmode="decimal"
              class="font-mono"
              data-testid="plan-discount"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('discountPercent')"
            />
          </template>
        </FormField>

        <div class="grid grid-cols-1 gap-3 sm:grid-cols-3">
          <FormField
            field-name="defaultRpm"
            :label="t('plans.defaultRpm')"
            :input-id="defaultRpmId"
            :error="fieldError('defaultRpm')"
            :guide="t('plans.defaultRpmGuide')"
          >
            <template #default="{ hintId, invalid }">
              <FormTextInput
                :id="defaultRpmId"
                v-model="defaultRpm"
                type="text"
                inputmode="numeric"
                class="font-mono"
                data-testid="plan-default-rpm"
                :invalid="invalid"
                :hint-id="hintId"
                v-on="fieldInputHandlers('defaultRpm')"
              />
            </template>
          </FormField>
          <FormField
            field-name="sharedRpm"
            :label="t('plans.sharedRpm')"
            :input-id="sharedRpmId"
            :error="fieldError('sharedRpm')"
            :guide="t('plans.sharedRpmGuide')"
          >
            <template #default="{ hintId, invalid }">
              <FormTextInput
                :id="sharedRpmId"
                v-model="sharedRpm"
                type="text"
                inputmode="numeric"
                class="font-mono"
                data-testid="plan-shared-rpm"
                :invalid="invalid"
                :hint-id="hintId"
                v-on="fieldInputHandlers('sharedRpm')"
              />
            </template>
          </FormField>
          <FormField
            field-name="grantUsd"
            :label="t('plans.initialGrant')"
            :input-id="grantId"
            :error="fieldError('grantUsd')"
            :guide="t('plans.initialGrantGuide')"
          >
            <template #default="{ hintId, invalid }">
              <FormTextInput
                :id="grantId"
                v-model="grantUsd"
                type="text"
                inputmode="decimal"
                class="font-mono"
                data-testid="plan-grant"
                :invalid="invalid"
                :hint-id="hintId"
                v-on="fieldInputHandlers('grantUsd')"
              />
            </template>
          </FormField>
        </div>

        <FormField
          field-name="note"
          :label="t('plans.note')"
          :input-id="noteId"
          :error="fieldError('note')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="noteId"
              v-model="note"
              type="text"
              data-testid="plan-note"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('note')"
            />
          </template>
        </FormField>

        <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <FormField
            field-name="noteVisible"
            layout="inline"
            :label="t('plans.noteVisible')"
            :input-id="`${noteId}-visible`"
          >
            <FormSwitch
              :id="`${noteId}-visible`"
              v-model="noteVisible"
              data-testid="plan-note-visible"
            />
          </FormField>
          <FormField
            field-name="sharedWithAdmin"
            layout="inline"
            :label="t('plans.sharedWithAdmin')"
            :input-id="`${uid}-shared`"
            :guide="t('plans.sharedWithAdminGuide')"
          >
            <FormSwitch
              :id="`${uid}-shared`"
              v-model="sharedWithAdmin"
              data-testid="plan-shared-switch"
            />
          </FormField>
          <FormField
            v-if="!isEdit"
            field-name="isDefault"
            layout="inline"
            :label="t('plans.isDefault')"
            :input-id="`${uid}-default`"
            :guide="
              audience === 'admin' ? t('plans.isDefaultGuideAdmin') : t('plans.isDefaultGuideUser')
            "
          >
            <FormSwitch
              :id="`${uid}-default`"
              v-model="isDefault"
              data-testid="plan-default-switch"
            />
          </FormField>
        </div>

        <div>
          <div class="mb-2 flex flex-wrap items-center justify-between gap-2">
            <p class="form-field-label m-0">
              {{ t('plans.modelGroups') }}
              <span class="text-fg-muted font-normal" data-testid="plan-groups-count">
                {{ t('plans.groupsSelected', { count: selectedGroups.length }) }}
              </span>
            </p>
            <SearchInput
              :id="`plan-group-search-${uid}`"
              v-model="groupSearch"
              class="max-w-xs"
              data-testid="plan-group-search"
              :placeholder="t('plans.groupSearch')"
              :aria-label="t('plans.groupSearch')"
            />
          </div>
          <DataTablePanel class="h-56" data-testid="plan-group-list">
            <VirtualTable
              class="h-full"
              :rows="groupRows"
              :colspan="2"
              :columns="groupColumns"
              :get-row-key="(row) => row.name"
              :empty-title="
                allGroups.length === 0 ? t('plans.noModelGroups') : t('plans.groupSearchEmpty')
              "
            >
              <template #header>
                <TableRow>
                  <TableHead class="w-10" />
                  <TableHead>{{ t('plans.modelGroups') }}</TableHead>
                </TableRow>
              </template>
              <template #row="{ row }">
                <TableRow data-testid="plan-group-row" :data-group-name="row.name">
                  <TableCell>
                    <Checkbox
                      :model-value="row.checked"
                      :data-testid="`plan-group-${row.name}`"
                      :aria-label="groupDisplayName(row.name, t('models.ungrouped'))"
                      @update:model-value="(checked: boolean) => toggleGroup(row.name, checked)"
                    />
                  </TableCell>
                  <TableCell truncate :title="row.name">
                    <span class="font-mono text-sm">
                      {{ groupDisplayName(row.name, t('models.ungrouped')) }}
                    </span>
                  </TableCell>
                </TableRow>
              </template>
            </VirtualTable>
          </DataTablePanel>
        </div>

        <!-- 用户档没有管理面：整块藏掉而不是禁用，避免暗示「这里将来能开」。 -->
        <fieldset v-if="showCapabilities" class="border-seed rounded-md border p-3">
          <legend class="text-fg-muted px-1 text-xs font-medium">
            {{ t('plans.capabilitiesTitle') }}
          </legend>
          <div class="grid grid-cols-1 gap-2 sm:grid-cols-2 lg:grid-cols-3">
            <label
              v-for="key in PLAN_CAPABILITY_KEYS"
              :key="key"
              :for="`plan-capability-${key}`"
              class="border-seed hover:bg-surface-alt flex cursor-pointer items-center gap-2 rounded-md border p-2 text-xs"
            >
              <Checkbox
                :id="`plan-capability-${key}`"
                :model-value="capabilities[key]"
                :data-testid="`plan-capability-${key}`"
                @update:model-value="(checked: boolean) => (capabilities[key] = checked)"
              />
              <span>{{ capabilityLabel(key) }}</span>
            </label>
          </div>
        </fieldset>
      </div>
      <div class="card-footer card-body flex justify-between gap-2">
        <button type="button" class="btn" @click="emit('close')">{{ t('common.cancel') }}</button>
        <button
          type="submit"
          class="btn btn-primary"
          data-testid="plan-save"
          :disabled="saveMutation.isPending.value"
        >
          {{ t('common.save') }}
        </button>
      </div>
    </form>
  </FloatingWindow>
</template>
