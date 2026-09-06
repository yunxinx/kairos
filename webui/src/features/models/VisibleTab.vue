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
import ChannelSourceMark from '@/features/models/ChannelSourceMark.vue';
import UnifiedJumpOrder from '@/features/models/UnifiedJumpOrder.vue';
import { useChannelDirectory } from '@/composables/useChannelDirectory';
import { callableRouteMembers } from '@/lib/unified-sources';
import {
  DEFAULT_MODEL_GROUP,
  previewVisibleModels,
  previewVisibleSections,
} from '@/lib/visible-models';

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
// 只读预览：走名录投影，admin 无渠道写权限也能正确渲染来源。
const { query: channelsQuery, channels, channelsKnown } = useChannelDirectory();
const ordersQuery = useQuery({
  queryKey: ['channel-model-orders'],
  queryFn: () => apiClient.listChannelModelOrders(),
});

const groupOptions = computed(() => {
  const groups = groupsQuery.data.value ?? [];
  const unified = unifiedQuery.data.value ?? [];
  return groups.map((group) => ({
    value: group.name,
    label: group.name === DEFAULT_MODEL_GROUP ? t('models.ungrouped') : group.name,
    count: previewVisibleModels(groups, unified, channels.value, group.name).visibleIds.length,
  }));
});

const sections = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  return previewVisibleSections(
    groupsQuery.data.value ?? [],
    unifiedQuery.data.value ?? [],
    channels.value,
    selectedGroups.value,
  )
    .map((section) => {
      const byId = new Map(section.unified.map((item) => [item.id, item]));
      const rows = section.visibleIds
        .filter((id) => !q || id.toLowerCase().includes(q))
        .map((id) => {
          const unified = byId.get(id);
          return {
            id,
            unified,
            callableRoute: unified
              ? []
              : callableRouteMembers(id, channels.value, ordersQuery.data.value ?? []),
          };
        });
      return {
        groupName: section.groupName,
        label:
          section.groupName === DEFAULT_MODEL_GROUP ? t('models.ungrouped') : section.groupName,
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
    (channelsQuery.isPending.value && !channelsQuery.data.value) ||
    (ordersQuery.isPending.value && !ordersQuery.data.value),
);

const loadError = computed(
  () =>
    groupsQuery.isError.value ||
    unifiedQuery.isError.value ||
    channelsQuery.isError.value ||
    ordersQuery.isError.value,
);

function loadErrorMessage(): string {
  if (groupsQuery.isError.value) return extractApiError(groupsQuery.error.value).message;
  if (unifiedQuery.isError.value) return extractApiError(unifiedQuery.error.value).message;
  if (channelsQuery.isError.value) return extractApiError(channelsQuery.error.value).message;
  return extractApiError(ordersQuery.error.value).message;
}

function refetchAll() {
  void groupsQuery.refetch();
  void unifiedQuery.refetch();
  void channelsQuery.refetch();
  void ordersQuery.refetch();
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
      <DataTable
        class="[&_[data-slot=table]]:table-fixed"
        data-testid="visible-table"
        :busy="showTableSkeleton"
      >
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
        <!-- 自动布局会把宽度让给双栏路由网格；固定后模型列吃到约三分之一，余量给请求路由。 -->
        <colgroup>
          <col class="w-[36%]" />
          <col />
        </colgroup>
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
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" :columns="2" />
          <template v-else>
            <template v-for="section in sections" :key="section.groupName">
              <TableRow
                class="inventory-section-row"
                data-testid="visible-section"
                :data-group="section.groupName"
              >
                <TableCell :colspan="2" class="inventory-section-cell">
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
                <TableCell class="min-w-0 font-mono font-medium">
                  <CopyableName :text="row.id" test-id="visible-model-name" />
                </TableCell>
                <TableCell class="min-w-0 whitespace-normal">
                  <div v-if="row.unified" data-testid="visible-unified-order">
                    <UnifiedJumpOrder
                      :members="row.unified.models"
                      :channels="channels"
                      :channels-known="channelsKnown"
                    />
                  </div>
                  <div
                    v-else-if="row.callableRoute.length > 0"
                    data-testid="visible-callable-order"
                  >
                    <UnifiedJumpOrder
                      :members="row.callableRoute"
                      :channels="channels"
                      :channels-known="channelsKnown"
                    />
                  </div>
                  <!-- 渠道表未到手时不敢断言「未登记」：那时路由本来就算不出来。 -->
                  <ChannelSourceMark v-else :kind="channelsKnown ? 'unlisted' : 'unknown'" />
                </TableCell>
              </TableRow>
            </template>
            <TableRow v-if="sections.length === 0">
              <TableCell :colspan="2" class="h-24 whitespace-normal">
                <EmptyState :title="t('models.visibleEmpty')" />
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
    </div>
  </div>
</template>
