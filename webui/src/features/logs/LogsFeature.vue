<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { LogQuery } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FilterField from '@/components/ui/FilterField.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import InlineError from '@/components/ui/InlineError.vue';
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

const PAGE_SIZE_OPTIONS = [10, 20, 50, 100, 200] as const;

const { t } = useI18n();

const draftTokenKey = ref('');
const draftModel = ref('');
const draftFrom = ref('');
const draftTo = ref('');
const appliedTokenKey = ref('');
const appliedModel = ref('');
const appliedFrom = ref('');
const appliedTo = ref('');
const page = ref(1);
const pageSize = ref(20);
const expandedIds = ref<Set<number>>(new Set());

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
});

function datetimeLocalToMillis(value: string): number | undefined {
  const trimmed = value.trim();
  if (!trimmed) {
    return undefined;
  }
  const millis = new Date(trimmed).getTime();
  return Number.isFinite(millis) ? millis : undefined;
}

function buildQuery(): LogQuery {
  const query: LogQuery = {
    page: page.value,
    page_size: pageSize.value,
  };
  const tokenKey = appliedTokenKey.value.trim();
  if (tokenKey) {
    query.token_key = tokenKey;
  }
  const model = appliedModel.value.trim();
  if (model) {
    query.model = model;
  }
  const fromMillis = datetimeLocalToMillis(appliedFrom.value);
  if (fromMillis !== undefined) {
    query.from_created_at = fromMillis;
  }
  const toMillis = datetimeLocalToMillis(appliedTo.value);
  if (toMillis !== undefined) {
    query.to_created_at = toMillis;
  }
  return query;
}

const logsQuery = useQuery({
  queryKey: ['logs', page, pageSize, appliedTokenKey, appliedModel, appliedFrom, appliedTo],
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

function applyFilters() {
  appliedTokenKey.value = draftTokenKey.value;
  appliedModel.value = draftModel.value;
  appliedFrom.value = draftFrom.value;
  appliedTo.value = draftTo.value;
  page.value = 1;
  expandedIds.value = new Set();
}

function clearFilters() {
  draftTokenKey.value = '';
  draftModel.value = '';
  draftFrom.value = '';
  draftTo.value = '';
  applyFilters();
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
  <div class="flex min-h-0 flex-1 flex-col overflow-hidden">
    <PageHeader :title="t('nav.logs')" />

    <InlineError
      v-if="logsQuery.isError.value && !logsQuery.data.value"
      :message="extractApiError(logsQuery.error.value).message"
      @retry="() => logsQuery.refetch()"
    />

    <div v-else class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <DataTable fill-viewport class="min-h-0 flex-1" :busy="showTableSkeleton">
        <template #toolbar>
          <DataTableToolbar class="items-end">
            <FilterField
              :label="t('logs.tokenKey')"
              input-id="logs-token-key"
              class="min-w-[12rem] flex-1"
            >
              <FormTextInput
                id="logs-token-key"
                v-model="draftTokenKey"
                type="text"
                class="h-8"
                :placeholder="t('logs.tokenKeyPlaceholder')"
              />
            </FilterField>
            <FilterField
              :label="t('logs.model')"
              input-id="logs-model"
              class="min-w-[10rem] flex-1"
            >
              <FormTextInput
                id="logs-model"
                v-model="draftModel"
                type="text"
                class="h-8"
                :placeholder="t('logs.modelPlaceholder')"
              />
            </FilterField>
            <FilterField :label="t('logs.from')" input-id="logs-from" class="min-w-[12rem]">
              <FormTextInput id="logs-from" v-model="draftFrom" type="datetime-local" class="h-8" />
            </FilterField>
            <FilterField :label="t('logs.to')" input-id="logs-to" class="min-w-[12rem]">
              <FormTextInput id="logs-to" v-model="draftTo" type="datetime-local" class="h-8" />
            </FilterField>
            <button
              type="button"
              class="btn btn-primary"
              data-testid="logs-apply-filters"
              @click="applyFilters"
            >
              {{ t('logs.applyFilters') }}
            </button>
            <button
              type="button"
              class="btn btn-subtle"
              data-testid="logs-clear-filters"
              @click="clearFilters"
            >
              {{ t('logs.clearFilters') }}
            </button>
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
