<script setup lang="ts">
// 用户余额快捷调整浮窗：与令牌编辑器余额调整保持高度一致的计算器机制与视觉规范。
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
}>();

const { t } = useI18n();
const { error, success } = useToast();
const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();

const uid = useId();
const amountInputId = `user-recharge-amount-${uid}`;

// 余额编辑计算器语义：输入框是基数（可直接改为目标余额），
// 快捷档位累计成右侧差额，`=` 后预览结果，保存时一并生效。
const displayedBalance = ref(props.user.balance_usd_micros);
const editorAmount = ref(formatUsdAmount(props.user.balance_usd_micros));
const quickDelta = ref(0);

const dirty = computed(
  () => editorAmount.value !== formatUsdAmount(displayedBalance.value) || quickDelta.value !== 0,
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

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

const saveMutation = useMutation({
  mutationFn: (delta: number) =>
    apiClient.rechargeUser(props.user.id, { delta_usd_micros: delta }),
  onSuccess: async () => {
    success(t('users.rechargeSuccess'));
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['users'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

function handleSave() {
  if (
    !validate(
      [{ name: 'amount', value: editorAmount.value, rules: [{ kind: 'required' }, { kind: 'usd', min: 0 }] }],
      t,
    )
  ) {
    return;
  }
  const delta = balanceDelta();
  if (delta === 0) {
    emit('close');
    return;
  }
  saveMutation.mutate(delta);
}

/** 快捷档位只累计差额、不改输入框、不立即落库；结果在 `=` 后预览，保存时生效。 */
function applyQuick(deltaUsd: number) {
  quickDelta.value += deltaUsd * 1_000_000;
}
</script>

<template>
  <form novalidate @submit.prevent="handleSave">
    <div class="card-body space-y-3">
      <fieldset class="border-seed rounded-md border p-3">
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
              :data-testid="`user-quick-add-${quick}`"
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
              :data-testid="`user-quick-sub-${quick}`"
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
                data-testid="user-recharge-amount"
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
              data-testid="user-balance-delta"
            >
              {{ quickDelta > 0 ? '+' : '-' }}{{ formatUsdAmount(Math.abs(quickDelta)) }}
            </p>
            <p class="text-fg-muted font-mono text-lg" aria-hidden="true">=</p>
            <p class="font-mono text-lg font-semibold" data-testid="user-balance-result">
              {{ formatUsdAmount(draftResult) }}
            </p>
            <button
              type="button"
              class="btn btn-sm"
              data-testid="user-quick-cancel"
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
      <button type="button" class="btn" @click="emit('close')">{{ t('common.cancel') }}</button>
      <button
        type="submit"
        class="btn btn-primary"
        data-testid="user-recharge-save"
        :disabled="saveMutation.isPending.value"
      >
        {{ t('common.save') }}
      </button>
    </div>
  </form>
</template>
