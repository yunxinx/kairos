<script setup lang="ts">
// 令牌余额调整浮窗：每个实例自持草稿，向窗口栈上报脏状态以供淘汰判定。
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { TokenRow } from '@/api/token-rows';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import type { BalanceMode } from '@/features/tokens/balance-mode';
import { parseUsdToMicros } from '@/lib/format';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const props = withDefaults(
  defineProps<{
    token: TokenRow;
    mode: BalanceMode;
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
const amountInputId = `token-balance-amount-${uid}`;

const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();

const balanceAmount = ref('');
const balanceError = ref('');

const dirty = computed(() => balanceAmount.value !== '');
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const balanceMutation = useMutation({
  mutationFn: (delta: number) =>
    apiClient.adjustTokenBalance(props.token.token_key, { delta_usd_micros: delta }),
  onSuccess: async () => {
    balanceError.value = '';
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['tokens'] });
  },
  onError: (err) => {
    balanceError.value = extractApiError(err).message;
  },
});

function handleBalance() {
  balanceError.value = '';
  if (
    !validate(
      [
        {
          name: 'amount',
          value: balanceAmount.value,
          rules: [{ kind: 'required' }, { kind: 'usd', min: 0 }],
        },
      ],
      t,
    )
  ) {
    return;
  }
  const micros = parseUsdToMicros(balanceAmount.value);
  if (micros === null || micros === 0) {
    balanceError.value = t('validation.usd');
    return;
  }
  const delta = props.mode === 'deduct' ? -micros : micros;
  balanceMutation.mutate(delta);
}
</script>

<template>
  <FloatingWindow
    :title="mode === 'recharge' ? t('tokens.rechargeTitle') : t('tokens.deductTitle')"
    :anchor="anchor"
    :stack-order="stackOrder"
    :cascade="cascade"
    :attention="attention"
    :topmost="topmost"
    @close="emit('close')"
    @pointerdown="emit('raise')"
  >
    <form novalidate @submit.prevent="handleBalance">
      <div class="card-body space-y-3">
        <FormField
          field-name="amount"
          :label="t('tokens.amount')"
          :input-id="amountInputId"
          :error="fieldError('amount')"
          :guide="t('tokens.amountGuide')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="amountInputId"
              v-model="balanceAmount"
              type="text"
              inputmode="decimal"
              class="font-mono"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('amount')"
            />
          </template>
        </FormField>
        <p v-if="balanceError" class="text-danger text-sm" data-testid="token-balance-error">
          {{ balanceError }}
        </p>
      </div>
      <div class="card-footer card-body flex justify-end gap-2">
        <button type="button" class="btn" @click="emit('close')">
          {{ t('common.cancel') }}
        </button>
        <button
          type="submit"
          class="btn btn-primary"
          data-testid="token-balance-save"
          :disabled="balanceMutation.isPending.value"
        >
          {{ t('common.save') }}
        </button>
      </div>
    </form>
  </FloatingWindow>
</template>
