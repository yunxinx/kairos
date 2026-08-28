<script setup lang="ts">
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { TokenBalanceCommand, TokenCreate, TokenUpdate } from '@/api/types';
import type { TokenRow } from '@/api/token-rows';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormSwitch from '@/components/ui/FormSwitch.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import ListboxSelect from '@/components/ui/ListboxSelect.vue';
import SegmentSwitch, { type SegmentPair } from '@/components/ui/SegmentSwitch.vue';
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
const balanceAmountInputId = `token-editor-balance-amount-${uid}`;

type BalanceMode = 'finite' | 'unlimited';
const QUICK_AMOUNTS = [1, 5, 10, 25, 50, 100] as const;

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

const initialMode: BalanceMode =
  props.initial?.balance_usd_micros === null ? 'unlimited' : 'finite';
const balanceMode = ref<BalanceMode>(initialMode);
const editorAmount = ref('');
const operationId = ref<string | null>(null);

const modeOptions = computed((): SegmentPair<BalanceMode> => [
  { value: 'finite', label: t('tokens.finite'), testId: 'token-balance-mode-finite' },
  { value: 'unlimited', label: t('common.unlimited'), testId: 'token-balance-mode-unlimited' },
]);

const amountMicros = computed(() => {
  const raw = editorAmount.value.trim();
  return raw === '' ? null : parseUsdToMicros(raw);
});

const expectedBalance = computed(() => {
  if (!props.initial) return null;
  const amount = amountMicros.value;
  if (balanceMode.value !== 'finite' || amount === null) return null;
  if (props.initial.balance_usd_micros === null) return amount;
  return props.initial.balance_usd_micros + amount;
});

function applyQuick(deltaUsd: number) {
  const base = amountMicros.value ?? 0;
  const next = base + deltaUsd * 1_000_000;
  editorAmount.value = formatUsdAmount(initialMode === 'unlimited' ? Math.max(0, next) : next);
}

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

const balanceDirty = computed(() => {
  if (props.initial === null) return editorInitialBalance.value !== initialBalance;
  return (
    balanceMode.value !== initialMode ||
    (balanceMode.value === 'finite' && editorAmount.value.trim() !== '')
  );
});

// 编辑有限额令牌时空白表示只改属性；从无限额切换为有限额必须填写初始余额，
// 有限额的零调整也不能伪装成一次成功的余额命令。
const balanceCommandReady = computed(() => {
  if (props.initial === null || balanceMode.value === 'unlimited') return true;
  if (editorAmount.value.trim() === '') return initialMode === 'finite';
  return initialMode === 'unlimited' || amountMicros.value !== 0;
});

const dirty = computed(
  () =>
    editorName.value !== initialName ||
    editorRpm.value !== initialRpm ||
    editorEnabled.value !== initialEnabled ||
    editorGroup.value !== initialGroup ||
    balanceDirty.value,
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

/** 保存载荷：新建由系统发 key，更新按库生成 id 定位。 */
type SavePayload =
  { kind: 'create'; body: TokenCreate } | { kind: 'update'; id: number; body: TokenUpdate };

const saveMutation = useMutation({
  mutationFn: async (payload: SavePayload) => {
    if (payload.kind === 'create') {
      return await apiClient.createToken(payload.body);
    }
    return await apiClient.updateToken(payload.id, payload.body);
  },
  onSuccess: async () => {
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['tokens'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

// 同一余额命令失败后可安全重试；动作或金额变化时必须换一个幂等键。
watch([balanceMode, editorAmount], () => {
  operationId.value = null;
  saveMutation.reset();
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
  } else if (balanceMode.value === 'finite' && editorAmount.value.trim() !== '') {
    const amountRule =
      initialMode === 'unlimited' ? ({ kind: 'usd', min: 0 } as const) : ({ kind: 'usd' } as const);
    specs.push({ name: 'amount', value: editorAmount.value, rules: [amountRule] });
  }
  if (!balanceCommandReady.value || !validate(specs, t)) return;
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
    const balanceChange: TokenBalanceCommand | null = (() => {
      if (balanceMode.value === 'unlimited') {
        if (initialMode === 'finite') {
          operationId.value ??= crypto.randomUUID();
          return { action: 'set_unlimited', operation_id: operationId.value };
        }
        return null;
      }
      if (editorAmount.value.trim() === '') return null;
      const amount = amountMicros.value;
      if (amount === null) return null;
      operationId.value ??= crypto.randomUUID();
      if (initialMode === 'unlimited') {
        return {
          action: 'set_finite',
          operation_id: operationId.value,
          balance_usd_micros: amount,
        };
      }
      if (amount !== 0) {
        return { action: 'adjust', operation_id: operationId.value, delta_usd_micros: amount };
      }
      return null;
    })();

    saveMutation.mutate({
      kind: 'update',
      id: props.initial.id,
      body: {
        name,
        rate_limit_rpm,
        enabled,
        model_group: editorGroup.value,
        ...(balanceChange === null ? {} : { balance_change: balanceChange }),
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
          <ListboxSelect
            :id="groupInputId"
            v-model="editorGroup"
            :options="groupOptions"
            :search-placeholder="t('tokens.modelGroup')"
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

        <!-- 编辑模式下合并余额调整面板 -->
        <template v-else>
          <div class="my-2 h-px bg-[var(--seed-border)]/60" />

          <div class="space-y-3">
            <div class="flex items-center justify-between gap-4">
              <div>
                <p class="text-fg-muted text-xs">{{ t('tokens.currentBalance') }}</p>
                <p class="font-mono text-base font-semibold" data-testid="token-current-balance">
                  {{
                    initial.balance_usd_micros === null
                      ? t('common.unlimited')
                      : formatUsdAmount(initial.balance_usd_micros)
                  }}
                </p>
              </div>
              <SegmentSwitch
                v-model="balanceMode"
                :options="modeOptions"
                :aria-label="t('tokens.balanceMode')"
                :disabled="saveMutation.isPending.value"
              />
            </div>

            <div v-if="balanceMode === 'finite'" class="space-y-3">
              <div class="flex flex-col gap-1.5">
                <div class="flex gap-1.5">
                  <button
                    v-for="quick in QUICK_AMOUNTS"
                    :key="quick"
                    type="button"
                    class="btn flex-1 font-mono text-xs"
                    :data-testid="`token-balance-quick-add-${quick}`"
                    :aria-label="t('tokens.quickIncrease', { amount: quick })"
                    :disabled="saveMutation.isPending.value"
                    @click="applyQuick(quick)"
                  >
                    +{{ quick }}
                  </button>
                </div>
                <div v-if="initialMode === 'finite'" class="flex gap-1.5">
                  <button
                    v-for="quick in QUICK_AMOUNTS"
                    :key="quick"
                    type="button"
                    class="btn flex-1 font-mono text-xs"
                    :data-testid="`token-balance-quick-sub-${quick}`"
                    :aria-label="t('tokens.quickDecrease', { amount: quick })"
                    :disabled="saveMutation.isPending.value"
                    @click="applyQuick(-quick)"
                  >
                    -{{ quick }}
                  </button>
                </div>
              </div>

              <FormField
                field-name="amount"
                :label="
                  initialMode === 'unlimited'
                    ? t('tokens.initialBalance')
                    : t('tokens.adjustmentAmount')
                "
                :input-id="balanceAmountInputId"
                :error="fieldError('amount')"
              >
                <template #default="{ hintId, invalid }">
                  <FormTextInput
                    :id="balanceAmountInputId"
                    v-model="editorAmount"
                    type="text"
                    inputmode="decimal"
                    class="font-mono"
                    data-testid="token-balance-amount"
                    :invalid="invalid"
                    :hint-id="hintId"
                    :disabled="saveMutation.isPending.value"
                    v-on="fieldInputHandlers('amount')"
                  />
                </template>
              </FormField>

              <div
                v-if="expectedBalance !== null"
                class="bg-surface-alt flex items-center justify-between rounded-md px-3 py-2"
                aria-live="polite"
              >
                <span class="text-fg-muted text-xs">{{ t('tokens.expectedBalance') }}</span>
                <span class="font-mono font-semibold" data-testid="token-expected-balance">
                  {{ formatUsdAmount(expectedBalance) }}
                </span>
              </div>
            </div>
          </div>
        </template>
      </div>
      <div class="card-footer card-body flex justify-between gap-2">
        <button type="button" class="btn" @click="emit('close')">
          {{ t('common.cancel') }}
        </button>
        <button
          type="submit"
          class="btn btn-primary"
          data-testid="token-save"
          :disabled="saveMutation.isPending.value || !balanceCommandReady"
        >
          {{ t('common.save') }}
        </button>
      </div>
    </form>
  </FloatingWindow>
</template>
