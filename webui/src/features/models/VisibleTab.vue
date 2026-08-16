<script setup lang="ts">
import { computed, ref } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import EmptyState from '@/components/ui/EmptyState.vue';
import InlineError from '@/components/ui/InlineError.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import { DEFAULT_MODEL_GROUP, previewVisibleModels } from '@/lib/visible-models';

const { t } = useI18n();
const groupName = ref(DEFAULT_MODEL_GROUP);

const groupsQuery = useQuery({
  queryKey: ['model-groups'],
  queryFn: () => apiClient.listModelGroups(),
});
const unifiedQuery = useQuery({
  queryKey: ['unified-models'],
  queryFn: () => apiClient.listUnifiedModels(),
});
const channelsQuery = useQuery({
  queryKey: ['channels'],
  queryFn: () => apiClient.listChannels(),
});

const groupOptions = computed(() =>
  (groupsQuery.data.value ?? []).map((group) => ({
    value: group.name,
    label: group.name === DEFAULT_MODEL_GROUP ? t('models.visibleUnbound') : group.name,
  })),
);

const preview = computed(() =>
  previewVisibleModels(
    groupsQuery.data.value ?? [],
    unifiedQuery.data.value ?? [],
    channelsQuery.data.value ?? [],
    groupName.value,
  ),
);

const visibleRows = computed(() => {
  const byId = new Map(preview.value.unified.map((item) => [item.id, item]));
  return preview.value.visibleIds.map((id) => ({
    id,
    unified: byId.get(id),
  }));
});

const loadError = computed(
  () => groupsQuery.isError.value || unifiedQuery.isError.value || channelsQuery.isError.value,
);

function loadErrorMessage(): string {
  if (groupsQuery.isError.value) return extractApiError(groupsQuery.error.value).message;
  if (unifiedQuery.isError.value) return extractApiError(unifiedQuery.error.value).message;
  return extractApiError(channelsQuery.error.value).message;
}

function refetchAll() {
  void groupsQuery.refetch();
  void unifiedQuery.refetch();
  void channelsQuery.refetch();
}
</script>

<template>
  <div class="flex flex-col gap-4">
    <InlineError v-if="loadError" :message="loadErrorMessage()" @retry="refetchAll" />
    <template v-else>
      <p class="text-fg-muted text-sm">{{ t('models.visibleLead') }}</p>
      <div class="max-w-sm">
        <label class="form-field-label mb-1 block" for="visible-group-select">
          {{ t('models.visibleGroup') }}
        </label>
        <UiSelect
          id="visible-group-select"
          v-model="groupName"
          :options="groupOptions"
          data-testid="visible-group-select"
        />
      </div>

      <EmptyState v-if="visibleRows.length === 0" :title="t('models.visibleEmpty')" />
      <ul v-else class="space-y-3" data-testid="visible-list">
        <li
          v-for="row in visibleRows"
          :key="row.id"
          class="border-seed rounded-md border p-3"
          data-testid="visible-model"
          :data-model="row.id"
        >
          <p class="font-mono text-sm font-medium">{{ row.id }}</p>
          <template v-if="row.unified">
            <p class="text-fg-muted mt-2 text-xs">{{ t('models.visibleOrder') }}</p>
            <ol
              class="mt-1 list-decimal pl-5 font-mono text-sm"
              data-testid="visible-unified-order"
            >
              <li v-for="member in row.unified.models" :key="member">{{ member }}</li>
            </ol>
            <p v-if="row.unified.hiddenMembers.length > 0" class="text-fg-muted mt-2 text-xs">
              {{ t('models.visibleHidden') }}:
              <span data-testid="visible-hidden-members">{{
                row.unified.hiddenMembers.join(', ')
              }}</span>
            </p>
          </template>
        </li>
      </ul>
    </template>
  </div>
</template>
