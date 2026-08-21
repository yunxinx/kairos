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

// 纯差额语义：输入框写的是「调整多少」，不是「调整成多少」。
//
// 接口本身是相对量（delta）。此前界面把输入框当成目标余额、保存时算
// `目标 - 打开窗口那刻的余额`——网关在持续扣费，期间的消耗会让实际结果偏离运营
// 输入的目标（丢更新）；而透支用户的预填值是负数，又会被「不小于 0」的校验挡住，
// 最需要充值的场景反而走不通。
const currentBalance = computed(() => props.user.balance_usd_micros);

/** 手输差额（美元字符串，可为负）。 */
const editorAmount = ref('');
/** 快捷档位累计的差额（micro-USD）。 */
const quickDelta = ref(0);

/** 手输部分的差额（micro-USD）；空串视为 0，非法输入为 null。 */
const typedDelta = computed(() => {
  const raw = editorAmount.value.trim();
  if (raw === '') return 0;
  return parseUsdToMicros(raw);
});

/** 本次要应用的总差额；输入非法时为 null。 */
const totalDelta = computed(() => {
  const typed = typedDelta.value;
  if (typed === null) return null;
  return typed + quickDelta.value;
});

/** 预计调整后的余额；允许为负（后端本就允许透支）。 */
const projectedBalance = computed(() => {
  if (totalDelta.value === null) return null;
  return currentBalance.value + totalDelta.value;
});

const dirty = computed(() => totalDelta.value !== null && totalDelta.value !== 0);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

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
  // 差额可正可负，不设下限；只拒绝解析不出来的输入。
  if (
    !validate([{ name: 'amount', value: editorAmount.value, rules: [{ kind: 'usd' }] }], t)
  ) {
    return;
  }
  const delta = totalDelta.value;
  if (delta === null || delta === 0) {
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
        <p class="text-fg-muted mt-1 text-center text-xs" data-testid="user-current-balance">
          {{ t('users.currentBalance') }}
          <span class="text-fg font-mono font-semibold">{{
            formatUsdAmount(currentBalance)
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
            v-if="totalDelta !== null && totalDelta !== 0 && projectedBalance !== null"
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
              {{ formatUsdAmount(projectedBalance) }}
            </p>
            <button
              v-if="quickDelta !== 0"
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
