<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { LogQuery } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import DateRangePicker from '@/components/ui/DateRangePicker.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
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
import { LOGS_INITIAL_PAGE, LOGS_INITIAL_PAGE_SIZE } from '@/lib/admin-query-defaults';
import type { DateRange } from '@/lib/date-range';
import { scrollMainToTop } from '@/lib/main-scroll';

const PAGE_SIZE_OPTIONS = [10, 20, 50, 100, 200] as const;

const { t } = useI18n();

const draftKeyword = ref('');
const appliedKeyword = ref('');
const appliedRange = ref<DateRange>({ from: null, to: null });
const page = ref(LOGS_INITIAL_PAGE);
const pageSize = ref(LOGS_INITIAL_PAGE_SIZE);
const expandedIds = ref<Set<number>>(new Set());

const appliedFrom = computed(() => appliedRange.value.from);
const appliedTo = computed(() => appliedRange.value.to);

const pageSizeModel = computed({
  get: () => String(pageSize.value),
  set: (value: string) => {
    const parsed = Number.parseInt(value, 10);
    if (Number.isNaN(parsed) || parsed === pageSize.value) {
      return;
    }
    pageSize.value = parsed;
    page.value = 1;
    expandedIds.value = new Set();
  },
});

const pageSizeOptions = computed(() =>
  PAGE_SIZE_OPTIONS.map((size) => ({
    value: String(size),
    label: String(size),
  })),
);

watch(page, () => {
  expandedIds.value = new Set();
  scrollMainToTop();
});

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
  return query;
}

const logsQuery = useQuery({
  queryKey: ['logs', page, pageSize, appliedKeyword, appliedFrom, appliedTo],
  queryFn: () => apiClient.queryLogs(buildQuery()),
});

const items = computed(() => logsQuery.data.value?.items ?? []);
const showTableSkeleton = computed(() => logsQuery.isPending.value && !logsQuery.data.value);
const total = computed(() => logsQuery.data.value?.total ?? 0);
const totalPages = computed(() => Math.max(1, Math.ceil(total.value / pageSize.value)));
const canGoPrevious = computed(() => page.value > 1);
const canGoNext = computed(() => page.value < totalPages.value && total.value > 0);

const paginationSummary = computed(() =>
  t('logs.paginationSummary', {
    page: page.value,
    totalPages: totalPages.value,
    total: total.value,
  }),
);

// 综合搜索防抖即时生效：与其他资源页的输入即过滤体验一致，避免每次按键都发请求。
const KEYWORD_DEBOUNCE_MS = 300;
let keywordTimer: number | undefined;

function resetResults() {
  page.value = 1;
  expandedIds.value = new Set();
}

function applyKeywordNow() {
  window.clearTimeout(keywordTimer);
  keywordTimer = undefined;
  if (appliedKeyword.value === draftKeyword.value) {
    return;
  }
  appliedKeyword.value = draftKeyword.value;
  resetResults();
}

watch(draftKeyword, () => {
  window.clearTimeout(keywordTimer);
  keywordTimer = window.setTimeout(applyKeywordNow, KEYWORD_DEBOUNCE_MS);
});

watch(appliedRange, resetResults);

onUnmounted(() => {
  window.clearTimeout(keywordTimer);
});

function clearFilters() {
  window.clearTimeout(keywordTimer);
  keywordTimer = undefined;
  draftKeyword.value = '';
  appliedKeyword.value = '';
  appliedRange.value = { from: null, to: null };
  resetResults();
}

function toggleExpand(id: number) {
  const next = new Set(expandedIds.value);
  if (next.has(id)) {
    next.delete(id);
  } else {
    next.add(id);
  }
  expandedIds.value = next;
}

function isExpanded(id: number): boolean {
  return expandedIds.value.has(id);
}
</script>

<template>
  <div class="flex flex-col">
    <PageHeader :title="t('nav.logs')" />

    <InlineError
      v-if="logsQuery.isError.value && !logsQuery.data.value"
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
              :detail-col-span="8"
              @toggle-expand="toggleExpand(entry.id)"
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
            :total-pages="totalPages"
            :summary="paginationSummary"
            page-size-id="logs-page-size"
            :page-size-options="pageSizeOptions"
            :can-previous="canGoPrevious"
            :can-next="canGoNext"
            summary-test-id="logs-pagination-summary"
            previous-test-id="logs-prev"
            next-test-id="logs-next"
          />
        </template>
      </DataTable>
    </div>
  </div>
</template>
