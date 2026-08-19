<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { SystemLogQuery } from '@/api/types';
import DateRangePicker from '@/components/ui/DateRangePicker.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FacetedFilter from '@/components/ui/FacetedFilter.vue';
import InlineError from '@/components/ui/InlineError.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import DataTable from '@/components/ui/data-table/DataTable.vue';
import DataTablePagination from '@/components/ui/data-table/DataTablePagination.vue';
import DataTableToolbar from '@/components/ui/data-table/DataTableToolbar.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import { useLogListControls } from '@/features/logs/useLogListControls';
import { formatUnixMillis } from '@/lib/format';

const LEVELS = ['error', 'warn', 'info'] as const;

const { t, locale } = useI18n();
const {
  draftKeyword,
  appliedKeyword,
  appliedRange,
  appliedFrom,
  appliedTo,
  page,
  pageSize,
  pageSizeModel,
  pageSizeOptions,
  applyKeywordNow,
  resetResults,
  clearBaseFilters,
  pagination,
} = useLogListControls();
const appliedLevels = ref<string[]>([]);
const appliedTargets = ref<string[]>([]);

const levelOptions = computed(() =>
  LEVELS.map((level) => ({
    value: level,
    label: t(`logs.levels.${level}`),
  })),
);

watch(appliedLevels, resetResults);
watch(appliedTargets, resetResults);

function buildQuery(): SystemLogQuery {
  const query: SystemLogQuery = {
    page: page.value,
    page_size: pageSize.value,
  };
  const keyword = appliedKeyword.value.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (appliedFrom.value !== null) {
    query.from_created_at = appliedFrom.value;
  }
  if (appliedTo.value !== null) {
    query.to_created_at = appliedTo.value;
  }
  if (appliedLevels.value.length > 0) {
    query.level = appliedLevels.value;
  }
  if (appliedTargets.value.length > 0) {
    query.target = appliedTargets.value;
  }
  return query;
}

const systemLogsQuery = useQuery({
  queryKey: [
    'system-logs',
    page,
    pageSize,
    appliedKeyword,
    appliedFrom,
    appliedTo,
    appliedLevels,
    appliedTargets,
  ],
  queryFn: () => apiClient.querySystemLogs(buildQuery()),
});

const items = computed(() => systemLogsQuery.data.value?.items ?? []);
const total = computed(() => systemLogsQuery.data.value?.total ?? 0);
const targetOptions = computed(() =>
  (systemLogsQuery.data.value?.targets ?? []).map((target) => ({
    value: target,
    label: target,
  })),
);
const showTableSkeleton = computed(
  () => systemLogsQuery.isPending.value && !systemLogsQuery.data.value,
);
const showError = computed(() => systemLogsQuery.isError.value && !systemLogsQuery.data.value);
const paging = computed(() => pagination(total.value));

function clearFilters() {
  appliedLevels.value = [];
  appliedTargets.value = [];
  clearBaseFilters();
}
</script>

<template>
  <InlineError
    v-if="showError"
    :message="extractApiError(systemLogsQuery.error.value).message"
    @retry="() => systemLogsQuery.refetch()"
  />

  <div v-else class="flex flex-col">
    <DataTable :busy="showTableSkeleton">
      <template #toolbar>
        <DataTableToolbar>
          <SearchInput
            id="system-logs-search"
            v-model="draftKeyword"
            class="max-w-sm"
            data-testid="system-logs-search"
            :placeholder="t('logs.systemSearchPlaceholder')"
            :aria-label="t('logs.search')"
            @keydown.enter="applyKeywordNow"
          />
          <FacetedFilter
            v-model="appliedLevels"
            :title="t('logs.levelFilter')"
            :options="levelOptions"
            test-id="system-logs-level-filter"
          />
          <FacetedFilter
            v-model="appliedTargets"
            :title="t('logs.targetFilter')"
            :options="targetOptions"
            test-id="system-logs-target-filter"
          />
          <template #actions>
            <DateRangePicker
              v-model="appliedRange"
              trigger-id="system-logs-time-range"
              trigger-test-id="system-logs-time-range"
              from-input-id="system-logs-from"
              to-input-id="system-logs-to"
            />
            <button
              type="button"
              class="btn btn-subtle"
              data-testid="system-logs-clear-filters"
              @click="clearFilters"
            >
              {{ t('logs.clearFilters') }}
            </button>
          </template>
        </DataTableToolbar>
      </template>
      <TableHeader>
        <TableRow>
          <TableHead>{{ t('logs.created') }}</TableHead>
          <TableHead>{{ t('logs.level') }}</TableHead>
          <TableHead>{{ t('logs.target') }}</TableHead>
          <TableHead>{{ t('logs.message') }}</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        <TableRowsSkeleton v-if="showTableSkeleton" :columns="4" />
        <template v-else>
          <TableRow
            v-for="entry in items"
            :key="entry.id"
            data-testid="system-log-row"
            :data-log-id="String(entry.id)"
          >
            <TableCell class="text-fg-muted font-mono text-xs">
              {{ formatUnixMillis(entry.created_at, locale) }}
            </TableCell>
            <TableCell data-testid="system-log-level">{{ entry.level }}</TableCell>
            <TableCell class="font-mono text-sm" data-testid="system-log-target">
              {{ entry.target }}
            </TableCell>
            <TableCell truncate :title="entry.message" data-testid="system-log-message">
              {{ entry.message }}
            </TableCell>
          </TableRow>
          <TableRow v-if="items.length === 0">
            <TableCell :colspan="4" class="h-24 whitespace-normal">
              <EmptyState data-testid="system-logs-empty" :title="t('common.emptyList')" />
            </TableCell>
          </TableRow>
        </template>
      </TableBody>
      <template #pagination>
        <DataTablePagination
          v-model:page="page"
          v-model:page-size="pageSizeModel"
          :total-pages="paging.totalPages"
          :summary="paging.summary"
          page-size-id="system-logs-page-size"
          :page-size-options="pageSizeOptions"
          :can-previous="paging.canGoPrevious"
          :can-next="paging.canGoNext"
          summary-test-id="system-logs-pagination-summary"
          previous-test-id="system-logs-prev"
          next-test-id="system-logs-next"
        />
      </template>
    </DataTable>
  </div>
</template>
