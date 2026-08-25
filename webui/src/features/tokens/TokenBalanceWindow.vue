<script setup lang="ts">
import { computed, ref, useId, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { TokenBalanceCommand } from '@/api/types';
import type { TokenRow } from '@/api/token-rows';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import SegmentSwitch, { type SegmentPair } from '@/components/ui/SegmentSwitch.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { useToast } from '@/composables/useToast';
import { formatUsdAmount, parseUsdToMicros } from '@/lib/format';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

type BalanceMode = 'finite' | 'unlimited';

const QUICK_AMOUNTS = [1, 5, 10, 25, 50, 100] as const;

const props = withDefaults(
  defineProps<{
    token: TokenRow;
    anchor?: FloatingWindowAnchor | null;
    stackOrder?: number;
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
const { error, success } = useToast();
const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();
const inputId = `token-balance-amount-${useId()}`;

const initialMode: BalanceMode = props.token.balance_usd_micros === null ? 'unlimited' : 'finite';
const mode = ref<BalanceMode>(initialMode);
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
  const amount = amountMicros.value;
  if (mode.value !== 'finite' || amount === null) return null;
  if (props.token.balance_usd_micros === null) return amount;
  return props.token.balance_usd_micros + amount;
});

const dirty = computed(
  () => mode.value !== initialMode || (mode.value === 'finite' && editorAmount.value.trim() !== ''),
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const canSave = computed(() => {
  if (mode.value === 'unlimited') return initialMode === 'finite';
  if (editorAmount.value.trim() === '') return false;
  const amount = amountMicros.value;
  return initialMode === 'unlimited' || amount !== 0;
});

const saveMutation = useMutation({
  mutationFn: (command: TokenBalanceCommand) =>
    apiClient.adjustTokenBalance(props.token.id, command),
  onSuccess: async () => {
    success(t('tokens.balanceUpdated'));
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['tokens'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

const isPending = computed(() => saveMutation.isPending.value);

watch([mode, editorAmount], () => {
  operationId.value = null;
  saveMutation.reset();
});

function handleSave() {
  if (!canSave.value) return;
  if (mode.value === 'unlimited') {
    operationId.value ??= crypto.randomUUID();
    saveMutation.mutate({ action: 'set_unlimited', operation_id: operationId.value });
    return;
  }
  const amountRule =
    initialMode === 'unlimited' ? ({ kind: 'usd', min: 0 } as const) : ({ kind: 'usd' } as const);
  if (!validate([{ name: 'amount', value: editorAmount.value, rules: [amountRule] }], t)) {
    return;
  }
  const amount = amountMicros.value;
  if (amount === null) return;
  operationId.value ??= crypto.randomUUID();
  if (initialMode === 'unlimited') {
    saveMutation.mutate({
      action: 'set_finite',
      operation_id: operationId.value,
      balance_usd_micros: amount,
    });
  } else {
    saveMutation.mutate({
      action: 'adjust',
      operation_id: operationId.value,
      delta_usd_micros: amount,
    });
  }
}

function applyQuick(deltaUsd: number) {
  const base = amountMicros.value ?? 0;
  const next = base + deltaUsd * 1_000_000;
  editorAmount.value = formatUsdAmount(initialMode === 'unlimited' ? Math.max(0, next) : next);
}
</script>

<template>
  <FloatingWindow
    :title="t('tokens.balanceTitle', { name: token.name })"
    :anchor="anchor"
    :stack-order="stackOrder"
    :cascade="cascade"
    :attention="attention"
    :topmost="topmost"
    :close-disabled="isPending"
    @close="emit('close')"
    @pointerdown="emit('raise')"
  >
    <form novalidate @submit.prevent="handleSave">
      <div class="card-body space-y-4">
        <div class="flex items-center justify-between gap-4">
          <div>
            <p class="text-fg-muted text-xs">{{ t('tokens.currentBalance') }}</p>
            <p class="font-mono text-lg font-semibold" data-testid="token-current-balance">
              {{
                token.balance_usd_micros === null
                  ? t('common.unlimited')
                  : formatUsdAmount(token.balance_usd_micros)
              }}
            </p>
          </div>
          <SegmentSwitch
            v-model="mode"
            :options="modeOptions"
            :aria-label="t('tokens.balanceMode')"
            :disabled="isPending"
          />
        </div>

        <div v-if="mode === 'finite'" class="space-y-3">
          <div class="flex flex-col gap-1.5">
            <div class="flex gap-1.5">
              <button
                v-for="quick in QUICK_AMOUNTS"
                :key="quick"
                type="button"
                class="btn flex-1 font-mono text-xs"
                :data-testid="`token-balance-quick-add-${quick}`"
                :aria-label="t('tokens.quickIncrease', { amount: quick })"
                :disabled="isPending"
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
                  :disabled="isPending"
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
            :input-id="inputId"
            :error="fieldError('amount')"
          >
            <template #default="{ hintId, invalid }">
              <FormTextInput
                :id="inputId"
                v-model="editorAmount"
                type="text"
                inputmode="decimal"
                class="font-mono"
                data-testid="token-balance-amount"
                :invalid="invalid"
                :hint-id="hintId"
                :disabled="isPending"
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

      <div class="card-footer card-body flex justify-between gap-2">
        <button type="button" class="btn" :disabled="isPending" @click="emit('close')">
          {{ t('common.cancel') }}
        </button>
        <button
          type="submit"
          class="btn btn-primary"
          data-testid="token-balance-save"
          :disabled="saveMutation.isPending.value || !canSave"
        >
          {{ t('common.save') }}
        </button>
      </div>
    </form>
  </FloatingWindow>
</template>
