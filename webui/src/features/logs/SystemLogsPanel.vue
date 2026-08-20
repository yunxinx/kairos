<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { SystemLogEntry, SystemLogQuery, SortDir } from '@/api/types';
import DateRangePicker from '@/components/ui/DateRangePicker.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FacetedFilter from '@/components/ui/FacetedFilter.vue';
import InlineError from '@/components/ui/InlineError.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import DataTable from '@/components/ui/data-table/DataTable.vue';
import DataTableColumnHeader from '@/components/ui/data-table/DataTableColumnHeader.vue';
import DataTablePagination from '@/components/ui/data-table/DataTablePagination.vue';
import DataTableToolbar from '@/components/ui/data-table/DataTableToolbar.vue';
import DataTableViewOptions from '@/components/ui/data-table/DataTableViewOptions.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import SystemLogDetailWindow from '@/features/logs/SystemLogDetailWindow.vue';
import { useLogListControls } from '@/features/logs/useLogListControls';
import { useColumnVisibility, type ColumnVisibilitySpec } from '@/composables/useColumnVisibility';
import { useWindowStack } from '@/composables/useWindowStack';
import { formatUnixMillis } from '@/lib/format';
import { anchorFromEvent } from '@/lib/window-anchor';

type SystemLogWindowPayload = {
  entry: SystemLogEntry;
};

type SystemLogColumnId = 'created' | 'level' | 'target' | 'message' | 'actions';

const SYSTEM_LOG_COLUMNS: ColumnVisibilitySpec<SystemLogColumnId>[] = [
  { id: 'created', locked: true },
  { id: 'level' },
  { id: 'target' },
  { id: 'message' },
  { id: 'actions', locked: true },
];

const SYSTEM_LOG_HIDEABLE: SystemLogColumnId[] = ['level', 'target', 'message'];

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

const {
  windows,
  topmostId,
  open: openWindow,
  close: closeWindow,
  bringToFront,
} = useWindowStack<SystemLogWindowPayload>();

const appliedLevels = ref<string[]>([]);
const appliedTargets = ref<string[]>([]);
const autoRefreshSeconds = ref<string>('0');

const { visible, columnCount, setVisible, menuItems } = useColumnVisibility(
  'kairos-system-logs-columns',
  SYSTEM_LOG_COLUMNS,
);

const columnMenuItems = computed(() => menuItems(SYSTEM_LOG_HIDEABLE));

const sortDir = ref<SortDir>('desc');

function onSortCreated(dir: SortDir) {
  sortDir.value = dir;
  resetResults();
}

function onClearCreatedSort() {
  sortDir.value = 'desc';
  resetResults();
}

const columnLabels = computed((): Record<SystemLogColumnId, string> => ({
  created: t('logs.created'),
  level: t('logs.level'),
  target: t('logs.target'),
  message: t('logs.message'),
  actions: t('common.actions'),
}));

const activeLogIds = computed(() => new Set(windows.value.map((win) => win.payload.entry.id)));

const levelOptions = computed(() =>
  LEVELS.map((level) => ({
    value: level,
    label: t(`logs.levels.${level}`),
  })),
);

const refreshOptions = computed(() => [
  { value: '0', label: t('logs.autoRefreshOff') },
  { value: '5', label: t('logs.autoRefreshSeconds', { seconds: 5 }) },
  { value: '10', label: t('logs.autoRefreshSeconds', { seconds: 10 }) },
  { value: '30', label: t('logs.autoRefreshSeconds', { seconds: 30 }) },
]);

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
  query.sort_by = 'created';
  query.sort_dir = sortDir.value;
  return query;
}

const refetchInterval = computed(() => {
  const s = Number.parseInt(autoRefreshSeconds.value, 10);
  return s > 0 ? s * 1000 : false;
});

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
    sortDir,
  ],
  queryFn: () => apiClient.querySystemLogs(buildQuery()),
  refetchInterval,
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

function openDetailWindow(event: MouseEvent, entry: SystemLogEntry) {
  const existing = windows.value.find((win) => win.payload.entry.id === entry.id);
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(anchorFromEvent(event), { entry });
}

function onQuickFilterTarget(target: string) {
  if (!appliedTargets.value.includes(target)) {
    appliedTargets.value = [...appliedTargets.value, target];
  }
}

function onQuickFilterLevel(level: string) {
  if (!appliedLevels.value.includes(level)) {
    appliedLevels.value = [...appliedLevels.value, level];
  }
}

function levelBadgeClass(level: string): string {
  switch (level.toLowerCase()) {
    case 'error':
      return 'badge-danger font-bold uppercase';
    case 'warn':
      return 'badge-warn font-semibold uppercase';
    case 'info':
      return 'badge-info uppercase';
    default:
      return 'uppercase';
  }
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
            <div class="flex items-center gap-1.5 text-xs text-[var(--fg-muted)]">
              <UiIcon
                name="refresh-cw"
                :size="14"
                :class="{
                  'animate-spin text-[var(--seed-primary)]':
                    autoRefreshSeconds !== '0' && systemLogsQuery.isFetching.value,
                }"
              />
              <div class="w-44 shrink-0">
                <UiSelect
                  id="system-logs-auto-refresh"
                  v-model="autoRefreshSeconds"
                  :options="refreshOptions"
                  :aria-label="t('logs.autoRefresh')"
                  class="text-xs"
                />
              </div>
            </div>
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
            <DataTableViewOptions
              :items="columnMenuItems"
              :labels="columnLabels"
              test-id="system-logs-columns"
              @toggle="setVisible"
            />
          </template>
        </DataTableToolbar>
      </template>

      <TableHeader>
        <TableRow>
          <TableHead class="w-44" :aria-sort="sortDir === 'asc' ? 'ascending' : 'descending'">
            <DataTableColumnHeader
              :label="t('logs.created')"
              :sorted="sortDir"
              :clearable="sortDir === 'asc'"
              @sort="onSortCreated"
              @clear="onClearCreatedSort"
            />
          </TableHead>
          <TableHead v-if="visible.level" class="w-24">{{ t('logs.level') }}</TableHead>
          <TableHead v-if="visible.target" class="w-56">{{ t('logs.target') }}</TableHead>
          <TableHead v-if="visible.message">{{ t('logs.message') }}</TableHead>
          <TableHead align="center">{{ t('common.actions') }}</TableHead>
        </TableRow>
      </TableHeader>

      <TableBody>
        <TableRowsSkeleton v-if="showTableSkeleton" :columns="columnCount" />
        <template v-else>
          <TableRow
            v-for="entry in items"
            :key="entry.id"
            data-testid="system-log-row"
            :data-log-id="String(entry.id)"
            class="group cursor-pointer transition-colors hover:bg-[var(--seed-surface-alt)]/60"
            :class="{ 'bg-[var(--seed-surface-alt)]/50 font-medium': activeLogIds.has(entry.id) }"
            @click="openDetailWindow($event, entry)"
          >
            <TableCell class="text-fg-muted font-mono text-xs whitespace-nowrap">
              {{ formatUnixMillis(entry.created_at, locale) }}
            </TableCell>
            <TableCell v-if="visible.level">
              <span
                class="badge text-[10px]"
                :class="levelBadgeClass(entry.level)"
                data-testid="system-log-level"
              >
                {{ entry.level }}
              </span>
            </TableCell>
            <TableCell
              v-if="visible.target"
              class="font-mono text-xs"
              data-testid="system-log-target"
            >
              <div class="inline-flex items-center gap-1">
                <span class="code-chip rounded px-1.5 py-0.5">{{ entry.target }}</span>
                <button
                  type="button"
                  class="rounded p-0.5 text-[var(--fg-muted)] opacity-0 transition-opacity group-hover:opacity-100 hover:text-[var(--seed-primary)]"
                  :title="t('logs.targetFilter')"
                  @click.stop="onQuickFilterTarget(entry.target)"
                >
                  <UiIcon name="filter" :size="11" />
                </button>
              </div>
            </TableCell>
            <TableCell
              v-if="visible.message"
              truncate
              :title="entry.message"
              data-testid="system-log-message"
              class="font-mono text-xs text-[var(--seed-fg)]"
            >
              {{ entry.message }}
            </TableCell>
            <TableCell align="center">
              <button
                type="button"
                class="btn btn-ghost btn-icon"
                :aria-label="t('logs.expandDetails')"
                :title="t('logs.expandDetails')"
                @click.stop="openDetailWindow($event, entry)"
              >
                <UiIcon name="external-link" :size="15" />
              </button>
            </TableCell>
          </TableRow>

          <TableRow v-if="items.length === 0">
            <TableCell :colspan="columnCount" class="h-24 whitespace-normal">
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

    <template v-for="(win, index) in windows" :key="win.id">
      <SystemLogDetailWindow
        :entry="win.payload.entry"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @filter-target="onQuickFilterTarget"
        @filter-level="onQuickFilterLevel"
      />
    </template>
  </div>
</template>
