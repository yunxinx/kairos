<script setup lang="ts">
// 令牌编辑器浮窗：定义字段 + 启用开关 + 余额快捷调整（并入编辑，按用户钱包充扣）。
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import { roleAtLeast, type Token, type TokenCreate } from '@/api/types';
import type { TokenRow } from '@/api/token-rows';
import FieldInfoHint from '@/components/ui/FieldInfoHint.vue';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormSwitch from '@/components/ui/FormSwitch.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { useToast } from '@/composables/useToast';
import { formatUsdAmount, parseUsdToMicros } from '@/lib/format';
import { useCurrentUser } from '@/lib/session';
import {
  DEFAULT_MODEL_GROUP,
  assignedGroupOptions,
  groupSelectOptions,
  tokenGroupUsable,
} from '@/lib/visible-models';
import type { FieldValidationSpec } from '@/lib/form-validation';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

/** 余额调整的快捷金额档（美元），按加/减两行呈现，点击累计进差额。 */
const QUICK_AMOUNTS = [1, 5, 10, 25, 50, 100] as const;

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
const amountInputId = `token-editor-amount-${uid}`;

const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();
const me = useCurrentUser();

const canListAllGroups = computed(() => {
  const role = me.value?.role;
  return role !== undefined && roleAtLeast(role, 'admin');
});
const canAdjustBalance = computed(() => {
  const role = me.value?.role;
  return role === 'root' || role === 'admin';
});

const groupsQuery = useQuery({
  queryKey: ['model-groups'],
  queryFn: () => apiClient.listModelGroups(),
  enabled: canListAllGroups,
});

const initialKey = props.initial ? props.initial.token_key : '';
const initialName = props.initial ? props.initial.name : '';
const initialRpm =
  props.initial && props.initial.rate_limit_rpm !== null
    ? String(props.initial.rate_limit_rpm)
    : '';
const initialEnabled = props.initial ? props.initial.enabled : true;
const initialGroup = (() => {
  if (props.initial) return props.initial.model_group;
  const user = me.value;
  if (user?.role === 'user') {
    if (user.assigned_groups.includes(DEFAULT_MODEL_GROUP)) return DEFAULT_MODEL_GROUP;
    return user.assigned_groups[0] ?? '';
  }
  return DEFAULT_MODEL_GROUP;
})();

const editorName = ref(initialName);
const editorRpm = ref(initialRpm);
const editorEnabled = ref(initialEnabled);
const editorGroup = ref(initialGroup);

const groupOptions = computed(() => {
  const user = me.value;
  if (user?.role === 'user') {
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

// 余额编辑：仅编辑已有令牌时可用。计算器语义——输入框是基数（可直接改为目标余额），
// 快捷档位累计成右侧差额，`=` 后预览结果，保存时一并生效。
const displayedBalance = ref(props.initial ? props.initial.balance_usd_micros : 0);
const editorAmount = ref(props.initial ? formatUsdAmount(props.initial.balance_usd_micros) : '0');
const quickDelta = ref(0);

const dirty = computed(
  () =>
    editorName.value !== initialName ||
    editorRpm.value !== initialRpm ||
    editorEnabled.value !== initialEnabled ||
    editorGroup.value !== initialGroup ||
    (canAdjustBalance.value &&
      (editorAmount.value !== formatUsdAmount(displayedBalance.value) || quickDelta.value !== 0)),
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

/** 保存载荷：新建不带 key（系统生成），更新携带原 key。 */
type SavePayload = { kind: 'create'; body: TokenCreate } | { kind: 'update'; body: Token };

const saveMutation = useMutation({
  mutationFn: (payload: SavePayload) =>
    payload.kind === 'create'
      ? apiClient.createToken(payload.body)
      : apiClient.updateToken(payload.body.token_key, payload.body),
  onSuccess: async () => {
    // 余额变更随保存一同生效：定义保存成功后，有差额才调用余额接口。
    const delta = props.initial === null || !canAdjustBalance.value ? 0 : balanceDelta();
    if (delta !== 0) {
      balanceMutation.mutate(delta);
      return;
    }
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['tokens'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

const balanceMutation = useMutation({
  mutationFn: (delta: number) =>
    apiClient.adjustTokenBalance(props.initial?.token_key ?? '', { delta_usd_micros: delta }),
  onSuccess: async () => {
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['tokens'] });
  },
  onError: async (err) => {
    error(extractApiError(err).message);
    // 定义字段此时已保存成功：同步列表，避免窗口内外状态分叉。
    await queryClient.invalidateQueries({ queryKey: ['tokens'] });
  },
});

/** 输入框基数（micro-USD）；非法输入返回 null。 */
const baseMicros = computed(() => parseUsdToMicros(editorAmount.value));

/** 计算器结果：基数叠加快捷差额，不低于 0；基数非法时为 null。 */
const draftResult = computed(() => {
  if (baseMicros.value === null) return null;
  return Math.max(0, baseMicros.value + quickDelta.value);
});

/** 保存时需要应用到余额的差额（micro-USD）；无变化或输入非法时为 0。 */
function balanceDelta(): number {
  if (draftResult.value === null) return 0;
  return draftResult.value - displayedBalance.value;
}

function handleSave() {
  const specs: FieldValidationSpec[] = [
    { name: 'name', value: editorName.value, rules: [{ kind: 'required' }] },
    { name: 'rateLimitRpm', value: editorRpm.value, rules: [{ kind: 'uint', min: 0 }] },
  ];
  if (props.initial !== null && canAdjustBalance.value) {
    specs.push({
      name: 'amount',
      value: editorAmount.value,
      rules: [{ kind: 'required' }, { kind: 'usd', min: 0 }],
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
        limit_usd_micros: null,
        rate_limit_rpm,
        enabled,
        model_group: editorGroup.value,
      },
    });
  } else {
    saveMutation.mutate({
      kind: 'update',
      body: {
        token_key: initialKey,
        name,
        limit_usd_micros: null,
        rate_limit_rpm,
        enabled,
        model_group: editorGroup.value,
      },
    });
  }
}

/** 快捷档位只累计差额、不改输入框、不立即落库；结果在 `=` 后预览，保存时生效。 */
function applyQuick(deltaUsd: number) {
  quickDelta.value += deltaUsd * 1_000_000;
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

        <fieldset
          v-if="initial !== null && canAdjustBalance"
          class="border-seed rounded-md border p-3"
        >
          <legend class="text-fg-muted flex items-center gap-1.5 px-1 text-xs font-medium">
            {{ t('tokens.balanceSection') }}
            <FieldInfoHint>
              <p class="field-info-hint-text">{{ t('tokens.amountGuide') }}</p>
            </FieldInfoHint>
          </legend>
          <div class="flex flex-col gap-1.5">
            <div class="flex gap-1.5">
              <button
                v-for="quick in QUICK_AMOUNTS"
                :key="quick"
                type="button"
                class="btn flex-1 font-mono text-xs"
                :data-testid="`token-quick-add-${quick}`"
                :aria-label="t('tokens.quickIncrease', { amount: quick })"
                @click="applyQuick(quick)"
              >
                +{{ quick }}
              </button>
            </div>
            <div class="flex gap-1.5">
              <button
                v-for="quick in QUICK_AMOUNTS"
                :key="quick"
                type="button"
                class="btn flex-1 font-mono text-xs"
                :data-testid="`token-quick-sub-${quick}`"
                :aria-label="t('tokens.quickDecrease', { amount: quick })"
                @click="applyQuick(-quick)"
              >
                -{{ quick }}
              </button>
            </div>
          </div>
          <div class="mt-2 flex items-end justify-center gap-3">
            <div class="w-1/3" data-form-field="amount">
              <div class="form-field-control">
                <FormTextInput
                  :id="amountInputId"
                  v-model="editorAmount"
                  type="text"
                  inputmode="decimal"
                  class="font-mono"
                  data-testid="token-editor-amount"
                  :invalid="Boolean(fieldError('amount'))"
                  :hint-id="`${amountInputId}-error`"
                  v-on="fieldInputHandlers('amount')"
                />
                <p
                  v-if="fieldError('amount')"
                  :id="`${amountInputId}-error`"
                  class="form-field-hint"
                  role="alert"
                >
                  {{ fieldError('amount') }}
                </p>
              </div>
            </div>
            <!-- 算式容器与输入框同高（2.25rem）并底对齐，数字恰好落在输入框中线上。 -->
            <div
              v-if="quickDelta !== 0 && draftResult !== null"
              class="flex h-9 items-center gap-3"
              aria-live="polite"
            >
              <p
                class="font-mono text-lg font-semibold"
                :class="quickDelta > 0 ? 'text-success' : 'text-danger'"
                data-testid="token-balance-delta"
              >
                {{ quickDelta > 0 ? '+' : '-' }}{{ formatUsdAmount(Math.abs(quickDelta)) }}
              </p>
              <p class="text-fg-muted font-mono text-lg" aria-hidden="true">=</p>
              <p class="font-mono text-lg font-semibold" data-testid="token-balance-result">
                {{ formatUsdAmount(draftResult) }}
              </p>
              <button
                type="button"
                class="btn btn-sm"
                data-testid="token-quick-cancel"
                :aria-label="t('tokens.quickCancel')"
                @click="quickDelta = 0"
              >
                {{ t('tokens.quickCancel') }}
              </button>
            </div>
          </div>
        </fieldset>
      </div>
      <div class="card-footer card-body flex justify-between gap-2">
        <button type="button" class="btn" @click="emit('close')">
          {{ t('common.cancel') }}
        </button>
        <button
          type="submit"
          class="btn btn-primary"
          data-testid="token-save"
          :disabled="saveMutation.isPending.value || balanceMutation.isPending.value"
        >
          {{ t('common.save') }}
        </button>
      </div>
    </form>
  </FloatingWindow>
</template>
