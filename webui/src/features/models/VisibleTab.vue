<script setup lang="ts">
import { computed, ref } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import CopyableName from '@/components/ui/CopyableName.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FacetedFilter from '@/components/ui/FacetedFilter.vue';
import InlineError from '@/components/ui/InlineError.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import DataTable from '@/components/ui/data-table/DataTable.vue';
import DataTableToolbar from '@/components/ui/data-table/DataTableToolbar.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import OverflowChips from '@/components/ui/OverflowChips.vue';
import { DEFAULT_MODEL_GROUP, previewVisibleSections } from '@/lib/visible-models';

const { t } = useI18n();
const selectedGroups = ref<string[]>([]);
const searchText = ref('');

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

const sections = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  return previewVisibleSections(
    groupsQuery.data.value ?? [],
    unifiedQuery.data.value ?? [],
    channelsQuery.data.value ?? [],
    selectedGroups.value,
  )
    .map((section) => {
      const byId = new Map(section.unified.map((item) => [item.id, item]));
      const rows = section.visibleIds
        .filter((id) => !q || id.toLowerCase().includes(q))
        .map((id) => ({
          id,
          unified: byId.get(id),
        }));
      return {
        groupName: section.groupName,
        label:
          section.groupName === DEFAULT_MODEL_GROUP
            ? t('models.visibleUnbound')
            : section.groupName,
        rows,
      };
    })
    .filter((section) => section.rows.length > 0 || selectedGroups.value.length > 0);
});

const visibleCount = computed(() => {
  const ids = new Set<string>();
  for (const section of sections.value) {
    for (const row of section.rows) ids.add(row.id);
  }
  return ids.size;
});

const showTableSkeleton = computed(
  () =>
    (groupsQuery.isPending.value && !groupsQuery.data.value) ||
    (unifiedQuery.isPending.value && !unifiedQuery.data.value) ||
    (channelsQuery.isPending.value && !channelsQuery.data.value),
);

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
  <div class="flex flex-col">
    <InlineError
      v-if="loadError && !groupsQuery.data.value"
      :message="loadErrorMessage()"
      @retry="refetchAll"
    />
    <div v-else class="flex flex-col">
      <DataTable :busy="showTableSkeleton">
        <template #toolbar>
          <DataTableToolbar>
            <SearchInput
              id="visible-search"
              v-model="searchText"
              class="max-w-sm"
              data-testid="visible-search"
              :placeholder="t('models.searchVisible')"
              :aria-label="t('models.searchVisible')"
            />
            <FacetedFilter
              v-model="selectedGroups"
              :title="t('models.visibleGroup')"
              :options="groupOptions"
              test-id="visible-group-filter"
            />
          </DataTableToolbar>
        </template>
        <TableHeader>
          <TableRow>
            <TableHead>
              <span class="inline-flex items-center gap-1.5">
                {{ t('pricing.model') }}
                <span
                  class="badge badge-neutral !bg-[var(--seed-surface)] font-mono"
                  data-testid="visible-model-count"
                  :aria-label="t('models.modelCount', { count: visibleCount })"
                >
                  {{ visibleCount }}
                </span>
              </span>
            </TableHead>
            <TableHead>{{ t('models.visibleOrder') }}</TableHead>
            <TableHead>{{ t('models.visibleHidden') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" :columns="3" />
          <template v-else>
            <template v-for="section in sections" :key="section.groupName">
              <TableRow
                class="inventory-section-row"
                data-testid="visible-section"
                :data-group="section.groupName"
              >
                <TableCell :colspan="3" class="inventory-section-cell">
                  {{ section.label }}
                </TableCell>
              </TableRow>
              <TableRow
                v-for="row in section.rows"
                :key="`${section.groupName}:${row.id}`"
                data-testid="visible-model"
                :data-model="row.id"
                :data-group="section.groupName"
              >
                <TableCell class="font-mono font-medium">
                  <CopyableName :text="row.id" test-id="visible-model-name" />
                </TableCell>
                <TableCell>
                  <span
                    v-if="row.unified"
                    class="font-mono text-sm"
                    data-testid="visible-unified-order"
                  >
                    {{ row.unified.models.join(' → ') }}
                  </span>
                  <span v-else class="text-fg-muted">{{ t('common.emptyCell') }}</span>
                </TableCell>
                <TableCell
                  :data-testid="
                    row.unified && row.unified.hiddenMembers.length > 0
                      ? 'visible-hidden-members'
                      : undefined
                  "
                >
                  <OverflowChips
                    v-if="row.unified && row.unified.hiddenMembers.length > 0"
                    :items="row.unified.hiddenMembers"
                  />
                  <span v-else class="text-fg-muted">{{ t('common.emptyCell') }}</span>
                </TableCell>
              </TableRow>
            </template>
            <TableRow v-if="sections.length === 0">
              <TableCell :colspan="3" class="h-24 whitespace-normal">
                <EmptyState :title="t('models.visibleEmpty')" />
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
    </div>
  </div>
</template>
