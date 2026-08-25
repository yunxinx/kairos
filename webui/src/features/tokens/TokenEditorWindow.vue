<script setup lang="ts">
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { TokenCreate, TokenUpdate } from '@/api/types';
import type { TokenRow } from '@/api/token-rows';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormSwitch from '@/components/ui/FormSwitch.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { useToast } from '@/composables/useToast';
import { parseUsdToMicros } from '@/lib/format';
import { useCurrentUser } from '@/lib/session';
import {
  DEFAULT_MODEL_GROUP,
  assignedGroupOptions,
  groupSelectOptions,
  tokenGroupUsable,
} from '@/lib/visible-models';
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
const { error } = useToast();

const uid = useId();
const nameInputId = `token-editor-name-${uid}`;
const groupInputId = `token-editor-group-${uid}`;
const rpmInputId = `token-editor-rpm-${uid}`;
const enabledInputId = `token-editor-enabled-${uid}`;
const initialBalanceInputId = `token-editor-initial-balance-${uid}`;

const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();
const me = useCurrentUser();

const canListAllGroups = computed(() => me.value?.role === 'root');
const groupsQuery = useQuery({
  queryKey: ['model-groups'],
  queryFn: () => apiClient.listModelGroups(),
  enabled: canListAllGroups,
});

const initialName = props.initial ? props.initial.name : '';
const initialBalance = '';
const initialRpm =
  props.initial && props.initial.rate_limit_rpm !== null
    ? String(props.initial.rate_limit_rpm)
    : '';
const initialEnabled = props.initial ? props.initial.enabled : true;
const initialGroup = (() => {
  if (props.initial) return props.initial.model_group;
  const user = me.value;
  if (user && user.role !== 'root') {
    if (user.assigned_groups.includes(DEFAULT_MODEL_GROUP)) return DEFAULT_MODEL_GROUP;
    return user.assigned_groups[0] ?? '';
  }
  return DEFAULT_MODEL_GROUP;
})();

const editorName = ref(initialName);
const editorInitialBalance = ref(initialBalance);
const editorRpm = ref(initialRpm);
const editorEnabled = ref(initialEnabled);
const editorGroup = ref(initialGroup);

const groupOptions = computed(() => {
  const user = me.value;
  if (user && user.role !== 'root') {
    return assignedGroupOptions(
      user.assigned_groups,
      editorGroup.value,
      t('models.ungrouped'),
      props.initial !== null,
    );
  }
  return groupSelectOptions(groupsQuery.data.value ?? [], editorGroup.value, t('models.ungrouped'));
});

const groupUnusable = computed(() => {
  const user = me.value;
  if (!user) return false;
  return !tokenGroupUsable(editorGroup.value, user.role, user.assigned_groups);
});

const dirty = computed(
  () =>
    editorName.value !== initialName ||
    (props.initial === null && editorInitialBalance.value !== initialBalance) ||
    editorRpm.value !== initialRpm ||
    editorEnabled.value !== initialEnabled ||
    editorGroup.value !== initialGroup,
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

/** 保存载荷：新建由系统发 key，更新按库生成 id 定位。 */
type SavePayload =
  { kind: 'create'; body: TokenCreate } | { kind: 'update'; id: number; body: TokenUpdate };

const saveMutation = useMutation({
  mutationFn: (payload: SavePayload) =>
    payload.kind === 'create'
      ? apiClient.createToken(payload.body)
      : apiClient.updateToken(payload.id, payload.body),
  onSuccess: async () => {
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['tokens'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

function handleSave() {
  const specs: FieldValidationSpec[] = [
    { name: 'name', value: editorName.value, rules: [{ kind: 'required' }] },
    { name: 'rateLimitRpm', value: editorRpm.value, rules: [{ kind: 'uint', min: 0 }] },
  ];
  if (props.initial === null) {
    specs.push({
      name: 'initialBalance',
      value: editorInitialBalance.value,
      rules: [{ kind: 'usd', min: 0 }],
    });
  }
  if (!validate(specs, t)) return;
  const name = editorName.value.trim();
  const rpmRaw = editorRpm.value.trim();
  const rate_limit_rpm = rpmRaw === '' ? null : Number(rpmRaw);
  const enabled = editorEnabled.value;
  if (props.initial === null) {
    saveMutation.mutate({
      kind: 'create',
      body: {
        name,
        balance_usd_micros:
          editorInitialBalance.value.trim() === ''
            ? null
            : parseUsdToMicros(editorInitialBalance.value),
        rate_limit_rpm,
        enabled,
        model_group: editorGroup.value,
      },
    });
  } else {
    saveMutation.mutate({
      kind: 'update',
      id: props.initial.id,
      body: {
        name,
        rate_limit_rpm,
        enabled,
        model_group: editorGroup.value,
      },
    });
  }
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
          field-name="modelGroup"
          :label="t('tokens.modelGroup')"
          :input-id="groupInputId"
          :guide="t('tokens.modelGroupGuide')"
        >
          <UiSelect
            :id="groupInputId"
            v-model="editorGroup"
            :options="groupOptions"
            data-testid="token-editor-group"
          />
          <p
            v-if="groupUnusable"
            class="text-danger mt-1 text-xs"
            data-testid="token-group-unusable-hint"
          >
            {{ t('tokens.groupUnusableHint') }}
          </p>
        </FormField>
        <FormField
          field-name="rateLimitRpm"
          :label="t('tokens.rateLimitRpm')"
          :input-id="rpmInputId"
          :error="fieldError('rateLimitRpm')"
          :guide="t('tokens.rateLimitRpmGuide')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="rpmInputId"
              v-model="editorRpm"
              type="text"
              inputmode="numeric"
              class="font-mono"
              data-testid="token-editor-rpm"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('rateLimitRpm')"
            />
          </template>
        </FormField>
        <FormField
          field-name="enabled"
          layout="inline"
          :label="t('tokens.enabled')"
          :input-id="enabledInputId"
          :error="fieldError('enabled')"
          :guide="t('tokens.enabledGuide')"
        >
          <FormSwitch
            :id="enabledInputId"
            v-model="editorEnabled"
            data-testid="token-enabled-switch"
          />
        </FormField>

        <FormField
          v-if="initial === null"
          field-name="initialBalance"
          :label="t('tokens.initialBalance')"
          :input-id="initialBalanceInputId"
          :error="fieldError('initialBalance')"
          :guide="t('tokens.initialBalanceGuide')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="initialBalanceInputId"
              v-model="editorInitialBalance"
              type="text"
              inputmode="decimal"
              class="font-mono"
              :placeholder="t('common.unlimited')"
              data-testid="token-editor-initial-balance"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('initialBalance')"
            />
          </template>
        </FormField>
      </div>
      <div class="card-footer card-body flex justify-between gap-2">
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
