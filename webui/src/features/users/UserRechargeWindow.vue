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
import { formatUsdMicros, parseUsdToMicros } from '@/lib/format';

const props = defineProps<{
  user: UserAdminView;
}>();

const emit = defineEmits<{
  close: [];
  'dirty-change': [dirty: boolean];
}>();

const { t } = useI18n();
const { error } = useToast();
const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();

const uid = useId();
const amountId = `user-recharge-amount-${uid}`;
const amount = ref('');

const dirty = computed(() => amount.value.trim() !== '');
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const saveMutation = useMutation({
  mutationFn: (delta: number) => apiClient.rechargeUser(props.user.id, { delta_usd_micros: delta }),
  onSuccess: async () => {
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
      [{ name: 'amount', value: amount.value, rules: [{ kind: 'required' }, { kind: 'usd' }] }],
      t,
    )
  ) {
    return;
  }
  const delta = parseUsdToMicros(amount.value);
  if (delta === null || delta === 0) return;
  saveMutation.mutate(delta);
}
</script>

<template>
  <form novalidate @submit.prevent="handleSave">
    <div class="card-body space-y-3">
      <p class="text-fg-muted text-sm">
        {{ t('users.currentBalance', { amount: formatUsdMicros(user.balance_usd_micros) }) }}
      </p>
      <FormField
        field-name="amount"
        :label="t('users.rechargeAmount')"
        :input-id="amountId"
        :error="fieldError('amount')"
        :guide="t('users.rechargeGuide')"
      >
        <template #default="{ hintId, invalid }">
          <FormTextInput
            :id="amountId"
            v-model="amount"
            type="text"
            inputmode="decimal"
            class="font-mono"
            data-testid="user-recharge-amount"
            :invalid="invalid"
            :hint-id="hintId"
            v-on="fieldInputHandlers('amount')"
          />
        </template>
      </FormField>
    </div>
    <div class="card-footer card-body flex justify-end gap-2">
      <button type="button" class="btn" @click="emit('close')">{{ t('common.cancel') }}</button>
      <button
        type="submit"
        class="btn btn-primary"
        data-testid="user-recharge-save"
        :disabled="saveMutation.isPending.value"
      >
        {{ t('users.recharge') }}
      </button>
    </div>
  </form>
</template>
