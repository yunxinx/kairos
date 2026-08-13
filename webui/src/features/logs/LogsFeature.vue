<script setup lang="ts">
import { computed, ref } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { LogQuery } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FilterField from '@/components/ui/FilterField.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import InlineError from '@/components/ui/InlineError.vue';
import DataTablePanel from '@/components/ui/DataTablePanel.vue';
import TableSkeleton from '@/components/ui/TableSkeleton.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import VirtualDataTable from '@/components/ui/VirtualDataTable.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import LogTableRow from '@/features/logs/LogTableRow.vue';
import type { FlatLogRow } from '@/features/logs/flat-log-row';
import { managementTableColumnPresets } from '@/lib/management-table-column-presets';

const PAGE_SIZE_OPTIONS = [10, 20, 50] as const;
const MAIN_ROW_HEIGHT = 52;
const DETAIL_ROW_HEIGHT = 280;

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
const total = computed(() => logsQuery.data.value?.total ?? 0);
const totalPages = computed(() => Math.max(1, Math.ceil(total.value / pageSize.value)));
const canGoPrevious = computed(() => page.value > 1);
const canGoNext = computed(() => page.value < totalPages.value && total.value > 0);

const flatRows = computed((): FlatLogRow[] => {
  const rows: FlatLogRow[] = [];
  for (let itemIndex = 0; itemIndex < items.value.length; itemIndex += 1) {
    rows.push({ kind: 'main', itemIndex });
    const item = items.value[itemIndex];
    if (item && expandedIds.value.has(item.id)) {
      rows.push({ kind: 'detail', itemIndex });
    }
  }
  return rows;
});

function rowHeight(index: number): number {
  return flatRows.value[index]?.kind === 'detail' ? DETAIL_ROW_HEIGHT : MAIN_ROW_HEIGHT;
}

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

function goToPrevious() {
  if (!canGoPrevious.value) {
    return;
  }
  page.value -= 1;
  expandedIds.value = new Set();
}

function goToNext() {
  if (!canGoNext.value) {
    return;
  }
  page.value += 1;
  expandedIds.value = new Set();
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

function resolvedRow(index: number) {
  const flatRow = flatRows.value[index];
  if (!flatRow) {
    return undefined;
  }
  const entry = items.value[flatRow.itemIndex];
  if (!entry) {
    return undefined;
  }
  return { flatRow, entry };
}
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col overflow-hidden">
    <PageHeader :title="t('nav.logs')" :subtitle="t('logs.subtitle')" />

    <div class="card mb-4 shrink-0">
      <div class="card-body flex flex-wrap items-end gap-3">
        <FilterField
          :label="t('logs.tokenKey')"
          input-id="logs-token-key"
          class="min-w-[12rem] flex-1"
        >
          <FormTextInput
            id="logs-token-key"
            v-model="draftTokenKey"
            type="text"
            :placeholder="t('logs.tokenKeyPlaceholder')"
          />
        </FilterField>
        <FilterField :label="t('logs.model')" input-id="logs-model" class="min-w-[10rem] flex-1">
          <FormTextInput
            id="logs-model"
            v-model="draftModel"
            type="text"
            :placeholder="t('logs.modelPlaceholder')"
          />
        </FilterField>
        <FilterField :label="t('logs.from')" input-id="logs-from" class="min-w-[12rem]">
          <FormTextInput id="logs-from" v-model="draftFrom" type="datetime-local" />
        </FilterField>
        <FilterField :label="t('logs.to')" input-id="logs-to" class="min-w-[12rem]">
          <FormTextInput id="logs-to" v-model="draftTo" type="datetime-local" />
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
      </div>
    </div>

    <TableSkeleton
      v-if="logsQuery.isPending.value"
      fill-viewport
      class="min-h-0 flex-1"
      :columns="8"
    />

    <InlineError
      v-else-if="logsQuery.isError.value"
      :message="extractApiError(logsQuery.error.value).message"
      @retry="() => logsQuery.refetch()"
    />

    <div v-else class="flex min-h-0 flex-1 flex-col gap-4 overflow-hidden">
      <DataTablePanel fill-viewport class="min-h-0 flex-1">
        <VirtualDataTable
          :row-count="flatRows.length"
          :columns="managementTableColumnPresets.logs"
          :estimate-row-height="MAIN_ROW_HEIGHT"
          :get-row-height="rowHeight"
        >
          <template #header>
            <TableHeader>
              <TableRow>
                <TableHead>{{ t('logs.created') }}</TableHead>
                <TableHead>{{ t('logs.token') }}</TableHead>
                <TableHead>{{ t('logs.model') }}</TableHead>
                <TableHead>{{ t('logs.channel') }}</TableHead>
                <TableHead>{{ t('logs.status') }}</TableHead>
                <TableHead>{{ t('logs.latency') }}</TableHead>
                <TableHead>{{ t('logs.cost') }}</TableHead>
                <TableHead class="w-10"
                  ><span class="sr-only">{{ t('logs.expand') }}</span></TableHead
                >
              </TableRow>
            </TableHeader>
          </template>
          <template #row="{ index, measureRow }">
            <LogTableRow
              v-if="resolvedRow(index)"
              :flat-row="resolvedRow(index)!.flatRow"
              :entry="resolvedRow(index)!.entry"
              :expanded="isExpanded(resolvedRow(index)!.entry.id)"
              :detail-col-span="8"
              :measure-row="measureRow"
              @toggle-expand="toggleExpand(resolvedRow(index)!.entry.id)"
            />
          </template>
          <template v-if="items.length === 0" #empty>
            <EmptyState data-testid="logs-empty" :title="t('common.emptyList')" />
          </template>
        </VirtualDataTable>
      </DataTablePanel>

      <div v-if="total > 0" class="flex shrink-0 flex-wrap items-center justify-between gap-3">
        <div class="flex flex-wrap items-center gap-3">
          <label class="text-fg-muted inline-flex items-center gap-2 text-sm" for="logs-page-size">
            <span>{{ t('logs.pageSize') }}</span>
            <UiSelect
              id="logs-page-size"
              v-model="pageSizeModel"
              class="min-w-[6rem]"
              :options="pageSizeOptions"
            />
          </label>
          <span class="text-fg-muted text-sm" data-testid="logs-pagination-summary">
            {{
              t('logs.paginationSummary', {
                page: page,
                totalPages: totalPages,
                total: total,
              })
            }}
          </span>
        </div>
        <div class="inline-flex gap-2">
          <button
            type="button"
            class="btn btn-sm btn-subtle"
            data-testid="logs-prev"
            :disabled="!canGoPrevious"
            @click="goToPrevious"
          >
            {{ t('common.previousPage') }}
          </button>
          <button
            type="button"
            class="btn btn-sm btn-subtle"
            data-testid="logs-next"
            :disabled="!canGoNext"
            @click="goToNext"
          >
            {{ t('common.nextPage') }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>
