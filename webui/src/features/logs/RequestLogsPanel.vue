<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { LogQuery, LogEntry } from '@/api/types';
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
import LogTableRow from '@/features/logs/LogTableRow.vue';
import { useLogListControls } from '@/features/logs/useLogListControls';
import { useToast } from '@/composables/useToast';

const { t } = useI18n();
const { error, success } = useToast();
const queryClient = useQueryClient();
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
const appliedSettled = ref<string[]>([]);
const expandedIds = ref<Set<number>>(new Set());
const details = ref<Map<number, LogEntry>>(new Map());
const detailLoading = ref<Set<number>>(new Set());
const detailErrors = ref<Map<number, string>>(new Map());

const settledOptions = computed(() => [
  { value: 'true', label: t('logs.settledYes') },
  { value: 'false', label: t('logs.settledNo') },
]);

watch(page, () => {
  expandedIds.value = new Set();
  details.value = new Map();
  detailLoading.value = new Set();
  detailErrors.value = new Map();
});

watch(appliedSettled, resetResults);

function buildQuery(): LogQuery {
  const query: LogQuery = {
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
  if (appliedSettled.value.length === 1) {
    query.settled = appliedSettled.value[0] === 'true';
  }
  return query;
}

const logsQuery = useQuery({
  queryKey: ['logs', page, pageSize, appliedKeyword, appliedFrom, appliedTo, appliedSettled],
  queryFn: () => apiClient.queryLogs(buildQuery()),
});

const closeMutation = useMutation({
  mutationFn: ({ id, action }: { id: number; action: 'settle' | 'waive' }) =>
    action === 'settle' ? apiClient.settleLog(id) : apiClient.waiveLog(id),
  onSuccess: async (_entry, vars) => {
    success(vars.action === 'settle' ? t('logs.settleSuccess') : t('logs.waiveSuccess'));
    details.value.delete(vars.id);
    await queryClient.invalidateQueries({ queryKey: ['logs'] });
    await queryClient.invalidateQueries({ queryKey: ['tokens'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

const closingId = computed(() =>
  closeMutation.isPending.value ? (closeMutation.variables.value?.id ?? null) : null,
);

const items = computed(() => logsQuery.data.value?.items ?? []);
const total = computed(() => logsQuery.data.value?.total ?? 0);
const unsettledTotal = computed(() => logsQuery.data.value?.unsettled_total ?? 0);
const showTableSkeleton = computed(() => logsQuery.isPending.value && !logsQuery.data.value);
const showError = computed(() => logsQuery.isError.value && !logsQuery.data.value);
const paging = computed(() => pagination(total.value));

function clearFilters() {
  appliedSettled.value = [];
  clearBaseFilters();
  expandedIds.value = new Set();
  details.value = new Map();
  detailLoading.value = new Set();
  detailErrors.value = new Map();
}

async function toggleExpand(id: number) {
  const next = new Set(expandedIds.value);
  if (next.has(id)) {
    next.delete(id);
    expandedIds.value = next;
    return;
  }
  next.add(id);
  expandedIds.value = next;
  if (details.value.has(id) || detailLoading.value.has(id)) {
    return;
  }
  const loading = new Set(detailLoading.value);
  loading.add(id);
  detailLoading.value = loading;
  try {
    const entry = await apiClient.getLog(id);
    const nextDetails = new Map(details.value);
    nextDetails.set(id, entry);
    details.value = nextDetails;
    const nextErrors = new Map(detailErrors.value);
    nextErrors.delete(id);
    detailErrors.value = nextErrors;
  } catch (err) {
    const nextErrors = new Map(detailErrors.value);
    nextErrors.set(id, extractApiError(err).message);
    detailErrors.value = nextErrors;
  } finally {
    const done = new Set(detailLoading.value);
    done.delete(id);
    detailLoading.value = done;
  }
}

function isExpanded(id: number): boolean {
  return expandedIds.value.has(id);
}
</script>

<template>
  <InlineError
    v-if="showError"
    :message="extractApiError(logsQuery.error.value).message"
    @retry="() => logsQuery.refetch()"
  />

  <div v-else class="flex flex-col">
    <DataTable :busy="showTableSkeleton">
      <template #toolbar>
        <DataTableToolbar>
          <SearchInput
            id="logs-search"
            v-model="draftKeyword"
            class="max-w-sm"
            data-testid="logs-search"
            :placeholder="t('logs.searchPlaceholder')"
            :aria-label="t('logs.search')"
            @keydown.enter="applyKeywordNow"
          />
          <FacetedFilter
            v-model="appliedSettled"
            :title="t('logs.settledFilter')"
            :options="settledOptions"
            test-id="logs-settled-filter"
          />
          <p
            v-if="unsettledTotal > 0"
            class="text-fg-muted text-sm"
            data-testid="logs-unsettled-total"
          >
            {{ t('logs.unsettledTotal', { count: unsettledTotal }) }}
          </p>
          <template #actions>
            <DateRangePicker
              v-model="appliedRange"
              trigger-id="logs-time-range"
              trigger-test-id="logs-time-range"
              from-input-id="logs-from"
              to-input-id="logs-to"
            />
            <button
              type="button"
              class="btn btn-subtle"
              data-testid="logs-clear-filters"
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
          <TableHead>{{ t('logs.token') }}</TableHead>
          <TableHead>{{ t('logs.model') }}</TableHead>
          <TableHead>{{ t('logs.channel') }}</TableHead>
          <TableHead>{{ t('logs.status') }}</TableHead>
          <TableHead>{{ t('logs.latency') }}</TableHead>
          <TableHead>{{ t('logs.cost') }}</TableHead>
          <TableHead class="w-10">
            <span class="sr-only">{{ t('logs.expand') }}</span>
          </TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        <TableRowsSkeleton v-if="showTableSkeleton" :columns="8" />
        <template v-else>
          <LogTableRow
            v-for="entry in items"
            :key="entry.id"
            :entry="entry"
            :expanded="isExpanded(entry.id)"
            :detail="details.get(entry.id) ?? null"
            :detail-loading="detailLoading.has(entry.id)"
            :detail-error="detailErrors.get(entry.id) ?? ''"
            :detail-col-span="8"
            :closing="closingId === entry.id"
            @toggle-expand="toggleExpand(entry.id)"
            @settle="closeMutation.mutate({ id: entry.id, action: 'settle' })"
            @waive="closeMutation.mutate({ id: entry.id, action: 'waive' })"
          />
          <TableRow v-if="items.length === 0">
            <TableCell :colspan="8" class="h-24 whitespace-normal">
              <EmptyState data-testid="logs-empty" :title="t('common.emptyList')" />
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
          page-size-id="logs-page-size"
          :page-size-options="pageSizeOptions"
          :can-previous="paging.canGoPrevious"
          :can-next="paging.canGoNext"
          summary-test-id="logs-pagination-summary"
          previous-test-id="logs-prev"
          next-test-id="logs-next"
        />
      </template>
    </DataTable>
  </div>
</template>
