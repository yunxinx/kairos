<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { UserAdminView } from '@/api/types';
import Checkbox from '@/components/ui/Checkbox.vue';
import InlineError from '@/components/ui/InlineError.vue';
import { useToast } from '@/composables/useToast';
import { groupDisplayName } from '@/lib/visible-models';

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

const selected = ref<string[]>([...props.user.assigned_groups]);

const groupsQuery = useQuery({
  queryKey: ['model-groups'],
  queryFn: () => apiClient.listModelGroups(),
});

const groupNames = computed(() => {
  const names = (groupsQuery.data.value ?? []).map((group) => group.name);
  for (const extra of selected.value) {
    if (!names.includes(extra)) names.push(extra);
  }
  return names.sort((left, right) => left.localeCompare(right));
});

const dirty = computed(() => {
  const next = [...selected.value].sort();
  const prev = [...props.user.assigned_groups].sort();
  return next.length !== prev.length || next.some((name, index) => name !== prev[index]);
});
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

function isSelected(name: string): boolean {
  return selected.value.includes(name);
}

function toggleGroup(name: string, checked: boolean) {
  if (checked) {
    if (!selected.value.includes(name)) selected.value = [...selected.value, name];
    return;
  }
  selected.value = selected.value.filter((item) => item !== name);
}

const saveMutation = useMutation({
  mutationFn: () => apiClient.replaceUserModelGroups(props.user.id, selected.value),
  onSuccess: async () => {
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['users'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});
</script>

<template>
  <div>
    <div class="card-body space-y-3">
      <InlineError
        v-if="groupsQuery.isError.value && !groupsQuery.data.value"
        :message="extractApiError(groupsQuery.error.value).message"
        @retry="() => groupsQuery.refetch()"
      />
      <ul v-else class="space-y-2">
        <li v-for="name in groupNames" :key="name" class="flex items-center gap-2">
          <Checkbox
            :model-value="isSelected(name)"
            :data-testid="`user-group-${name}`"
            @update:model-value="(checked) => toggleGroup(name, checked)"
          />
          <span class="text-sm">{{ groupDisplayName(name, t('models.ungrouped')) }}</span>
        </li>
      </ul>
    </div>
    <div class="card-footer card-body flex justify-end gap-2">
      <button type="button" class="btn" @click="emit('close')">{{ t('common.cancel') }}</button>
      <button
        type="button"
        class="btn btn-primary"
        data-testid="user-groups-save"
        :disabled="saveMutation.isPending.value || groupsQuery.isPending.value"
        @click="saveMutation.mutate()"
      >
        {{ t('common.save') }}
      </button>
    </div>
  </div>
</template>
