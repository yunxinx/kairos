<script setup lang="ts">
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { UserAdminView } from '@/api/types';
import FormField from '@/components/ui/FormField.vue';
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
</script>

<template>
  <form novalidate @submit.prevent="handleSave">
    <div class="card-body space-y-3">
      <div>
        <p class="text-fg-muted text-xs">{{ t('users.currentBalance') }}</p>
        <p class="font-mono text-base font-semibold" data-testid="user-current-balance">
          {{ formatUsdAmount(snapshotBalance) }}
        </p>
      </div>

      <div class="space-y-3">
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

        <FormField
          field-name="amount"
          :label="t('users.rechargeAmount')"
          :input-id="amountInputId"
          :error="fieldError('amount')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="amountInputId"
              v-model="editorAmount"
              type="text"
              inputmode="decimal"
              class="font-mono"
              data-testid="user-recharge-amount"
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
          <span class="text-fg-muted text-xs">{{ t('users.newBalance') }}</span>
          <span class="font-mono font-semibold" data-testid="user-balance-result">
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
        data-testid="user-recharge-save"
        :disabled="saveMutation.isPending.value || totalDelta === null || totalDelta === 0"
      >
        {{ t('common.save') }}
      </button>
    </div>
  </form>
</template>
