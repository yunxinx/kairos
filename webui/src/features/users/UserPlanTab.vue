<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { UserAdminView } from '@/api/types';
import InlineError from '@/components/ui/InlineError.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import { useToast } from '@/composables/useToast';

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
const plansQuery = useQuery({
  queryKey: ['plans'],
  queryFn: () => apiClient.listPlans(),
});

const currentPlanId = computed(() =>
  props.user.plan_id != null ? String(props.user.plan_id) : '',
);
const selectedPlanId = ref(currentPlanId.value);
const dirty = computed(() => selectedPlanId.value !== '' && selectedPlanId.value !== currentPlanId.value);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const planOptions = computed(() => {
  const options = (plansQuery.data.value ?? []).map((plan) => ({
    value: String(plan.id),
    label: plan.display_name,
  }));
  const current = currentPlanId.value;
  if (current && !options.some((option) => option.value === current)) {
    options.push({
      value: current,
      label: props.user.plan_display_name || current,
    });
  }
  return options;
});

const saveMutation = useMutation({
  mutationFn: () => apiClient.assignUserPlan(props.user.id, Number(selectedPlanId.value)),
  onSuccess: async () => {
    success(t('users.planUpdated'));
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['users'] });
    await queryClient.invalidateQueries({ queryKey: ['plans'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

function handleSave() {
  if (!dirty.value || !selectedPlanId.value) return;
  saveMutation.mutate();
}
</script>

<template>
  <div>
    <div class="card-body space-y-3">
      <InlineError
        v-if="plansQuery.isError.value && !plansQuery.data.value"
        :message="extractApiError(plansQuery.error.value).message"
        @retry="() => plansQuery.refetch()"
      />
      <template v-else>
        <div class="text-fg-muted text-xs leading-relaxed">
          {{ t('users.planTabGuide') }}
        </div>
        <div class="flex flex-col gap-3">
          <div>
            <p class="text-fg-muted text-xs font-medium">{{ t('plans.currentPlan') }}</p>
            <p class="font-mono text-sm font-semibold" data-testid="user-current-plan">
              {{ props.user.plan_display_name || t('common.unlimited') }}
            </p>
          </div>
          <label for="user-plan-select" class="flex flex-col gap-1 text-sm">
            <span class="text-fg-muted text-xs font-medium">{{ t('users.plan') }}</span>
            <UiSelect
              id="user-plan-select"
              v-model="selectedPlanId"
              :options="planOptions"
              data-testid="user-plan-select"
            />
          </label>
        </div>
      </template>
    </div>
    <div class="card-footer card-body flex justify-between gap-2">
      <button type="button" class="btn" @click="emit('close')">{{ t('common.cancel') }}</button>
      <button
        type="button"
        class="btn btn-primary"
        data-testid="user-plan-save"
        :disabled="saveMutation.isPending.value || !dirty"
        @click="handleSave"
      >
        {{ t('common.save') }}
      </button>
    </div>
  </div>
</template>
