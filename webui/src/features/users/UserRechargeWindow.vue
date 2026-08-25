<script setup lang="ts">
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { UserAdminView } from '@/api/types';
import FieldInfoHint from '@/components/ui/FieldInfoHint.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { useToast } from '@/composables/useToast';
import { formatUsdAmount, parseUsdToMicros } from '@/lib/format';

/** 余额调整的快捷金额档（美元），按加/减两行呈现，点击累计进差额。 */
const QUICK_AMOUNTS = [1, 5, 10, 25, 50, 100] as const;

const props = defineProps<{
  user: UserAdminView;
}>();

const emit = defineEmits<{
  close: [];
  'dirty-change': [dirty: boolean];
  'busy-change': [busy: boolean];
}>();

const { t } = useI18n();
const { error, success } = useToast();
const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();

const uid = useId();
const amountInputId = `user-recharge-amount-${uid}`;

// 当前余额只是预览基准；写接口接收本次相对调整量，不把快照换算成待写目标值。
const snapshotBalance = props.user.balance_usd_micros;
const editorAmount = ref('');

/** 本次相对调整量（micro-USD）；空串或非法输入为 null。 */
const totalDelta = computed(() => {
  const raw = editorAmount.value.trim();
  if (raw === '') return null;
  return parseUsdToMicros(raw);
});

const expectedBalance = computed(() => {
  const delta = totalDelta.value;
  return delta === null ? null : snapshotBalance + delta;
});

const dirty = computed(() => editorAmount.value.trim() !== '');
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

// 同一输入失败后重试复用 operation_id；金额变化代表新的用户意图。
const operationId = ref<string | null>(null);

const saveMutation = useMutation({
  mutationFn: (body: { operation_id: string; delta_usd_micros: number }) =>
    apiClient.adjustUserBalance(props.user.id, {
      ...body,
      reason: 'manual_adjustment',
    }),
  onSuccess: async () => {
    success(t('users.rechargeSuccess'));
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['users'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

const isPending = computed(() => saveMutation.isPending.value);
watch(isPending, (value) => emit('busy-change', value), { immediate: true });

watch(editorAmount, () => {
  operationId.value = null;
  saveMutation.reset();
});

function handleSave() {
  // 差额可正可负，不设下限；只拒绝解析不出来的输入。
  if (!validate([{ name: 'amount', value: editorAmount.value, rules: [{ kind: 'usd' }] }], t)) {
    return;
  }
  const delta = totalDelta.value;
  if (delta === null || delta === 0) return;
  operationId.value ??= crypto.randomUUID();
  saveMutation.mutate({ operation_id: operationId.value, delta_usd_micros: delta });
}

function applyQuick(deltaUsd: number) {
  const base = totalDelta.value ?? 0;
  editorAmount.value = formatUsdAmount(base + deltaUsd * 1_000_000);
}

function resetAdjustment() {
  editorAmount.value = '';
}
</script>

<template>
  <form novalidate @submit.prevent="handleSave">
    <div class="card-body space-y-3">
      <fieldset class="border-seed rounded-md border p-3">
        <legend class="text-fg-muted flex items-center gap-1.5 px-1 text-xs font-medium">
          {{ t('users.rechargeSection') }}
          <FieldInfoHint>
            <p class="field-info-hint-text">{{ t('users.rechargeGuide') }}</p>
          </FieldInfoHint>
        </legend>
        <div class="flex flex-col gap-1.5">
          <div class="flex gap-1.5">
            <button
              v-for="quick in QUICK_AMOUNTS"
              :key="quick"
              type="button"
              class="btn flex-1 font-mono text-xs"
              :data-testid="`user-quick-add-${quick}`"
              :aria-label="t('tokens.quickIncrease', { amount: quick })"
              :disabled="isPending"
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
              :data-testid="`user-quick-sub-${quick}`"
              :aria-label="t('tokens.quickDecrease', { amount: quick })"
              :disabled="isPending"
              @click="applyQuick(-quick)"
            >
              -{{ quick }}
            </button>
          </div>
        </div>
        <p class="text-fg-muted mt-1 text-center text-xs" data-testid="user-current-balance">
          {{ t('users.currentBalance') }}
          <span class="text-fg font-mono font-semibold">{{
            formatUsdAmount(snapshotBalance)
          }}</span>
        </p>
        <div class="mt-2 flex items-end justify-center gap-3">
          <div class="w-1/3" data-form-field="amount">
            <div class="form-field-control">
              <FormTextInput
                :id="amountInputId"
                v-model="editorAmount"
                type="text"
                inputmode="decimal"
                class="font-mono"
                data-testid="user-recharge-amount"
                :invalid="Boolean(fieldError('amount'))"
                :hint-id="`${amountInputId}-error`"
                :disabled="isPending"
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
            v-if="totalDelta !== null && totalDelta !== 0 && expectedBalance !== null"
            class="flex h-9 items-center gap-3"
            aria-live="polite"
          >
            <p
              class="font-mono text-lg font-semibold"
              :class="totalDelta > 0 ? 'text-success' : 'text-danger'"
              data-testid="user-balance-delta"
            >
              {{ totalDelta > 0 ? '+' : '-' }}{{ formatUsdAmount(Math.abs(totalDelta)) }}
            </p>
            <p class="text-fg-muted font-mono text-lg" aria-hidden="true">→</p>
            <p class="font-mono text-lg font-semibold" data-testid="user-balance-result">
              {{ formatUsdAmount(expectedBalance) }}
            </p>
            <button
              type="button"
              class="btn btn-sm"
              data-testid="user-quick-cancel"
              :aria-label="t('users.rechargeReset')"
              :disabled="isPending"
              @click="resetAdjustment"
            >
              {{ t('users.rechargeReset') }}
            </button>
          </div>
        </div>
      </fieldset>
    </div>

    <div class="card-footer card-body flex justify-between gap-2">
      <button type="button" class="btn" :disabled="isPending" @click="emit('close')">
        {{ t('common.cancel') }}
      </button>
      <button
        type="submit"
        class="btn btn-primary"
        data-testid="user-recharge-save"
        :disabled="saveMutation.isPending.value || totalDelta === null || totalDelta === 0"
      >
        {{ t('common.save') }}
      </button>
    </div>
  </form>
</template>
