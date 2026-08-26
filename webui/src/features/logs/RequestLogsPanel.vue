<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import {
  PROTOCOLS,
  type LogEntry,
  type LogQuery,
  type RequestLogSortBy,
  type SortDir,
} from '@/api/types';
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
import LogTableRow, { type RequestLogVisibleColumns } from '@/features/logs/LogTableRow.vue';
import RequestLogDetailWindow from '@/features/logs/RequestLogDetailWindow.vue';
import RequestLogBodyWindow from '@/features/logs/RequestLogBodyWindow.vue';
import { useLogListControls } from '@/features/logs/useLogListControls';
import { useChannelDirectory } from '@/composables/useChannelDirectory';
import { useColumnVisibility, type ColumnVisibilitySpec } from '@/composables/useColumnVisibility';
import { useWindowStack } from '@/composables/useWindowStack';
import { useToast } from '@/composables/useToast';
import { formatDiscountBp } from '@/lib/format';
import { hasCapability } from '@/lib/capabilities';
import { useCurrentUser } from '@/lib/session';
import { anchorFromEvent } from '@/lib/window-anchor';

type RequestLogWindowType = 'billing' | 'body';

type RequestLogWindowPayload = {
  type: RequestLogWindowType;
  entry: LogEntry;
};

type RequestLogColumnId =
  | 'created'
  | 'token'
  | 'model'
  | 'channel'
  | 'inboundProtocol'
  | 'tokens'
  | 'latency'
  | 'cache'
  | 'cacheHit'
  | 'cost'
  | 'billing'
  | 'body';

const REQUEST_LOG_COLUMNS: ColumnVisibilitySpec<RequestLogColumnId>[] = [
  { id: 'created', locked: true },
  { id: 'token' },
  { id: 'model' },
  { id: 'channel' },
  { id: 'inboundProtocol', defaultVisible: false },
  { id: 'tokens' },
  { id: 'latency' },
  { id: 'cache', defaultVisible: false },
  { id: 'cacheHit', defaultVisible: false },
  { id: 'cost' },
  { id: 'billing', locked: true },
  { id: 'body' },
];

const REQUEST_LOG_HIDEABLE: RequestLogColumnId[] = [
  'token',
  'model',
  'channel',
  'inboundProtocol',
  'tokens',
  'latency',
  'cache',
  'cacheHit',
  'cost',
  'body',
];

const { t } = useI18n();
const { error, success } = useToast();
const me = useCurrentUser();

/** 补扣/豁免是计费操作，要求生效能力 `settle_waive`；普通用户不渲染入口。 */
const canSettleLogs = computed(() => hasCapability(me.value, 'settle_waive'));
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

const {
  windows,
  topmostId,
  open: openWindow,
  close: closeWindow,
  bringToFront,
} = useWindowStack<RequestLogWindowPayload>();

const appliedSettled = ref<string[]>([]);
const appliedDiscountBp = ref<string[]>([]);
const appliedProtocols = ref<string[]>([]);
const appliedModel = ref<string | null>(null);
const appliedChannel = ref<string | null>(null);
const appliedTokenName = ref<string | null>(null);
const details = ref<Map<number, LogEntry>>(new Map());
const detailLoading = ref<Set<number>>(new Set());
const detailErrors = ref<Map<number, string>>(new Map());
const autoRefreshSeconds = ref<string>('0');

const { visible, columnCount, setVisible, menuItems } = useColumnVisibility(
  'kairos-logs-columns',
  REQUEST_LOG_COLUMNS,
);

const rowVisible = computed((): RequestLogVisibleColumns => ({
  token: visible.value.token,
  model: visible.value.model,
  channel: visible.value.channel,
  inboundProtocol: visible.value.inboundProtocol,
  tokens: visible.value.tokens,
  latency: visible.value.latency,
  cache: visible.value.cache,
  cacheHit: visible.value.cacheHit,
  cost: visible.value.cost,
  body: visible.value.body,
}));

const columnMenuItems = computed(() => menuItems(REQUEST_LOG_HIDEABLE));

const sortBy = ref<RequestLogSortBy>('created');
const sortDir = ref<SortDir>('desc');

const DEFAULT_SORT_BY: RequestLogSortBy = 'created';
const DEFAULT_SORT_DIR: SortDir = 'desc';

function isDefaultSort(): boolean {
  return sortBy.value === DEFAULT_SORT_BY && sortDir.value === DEFAULT_SORT_DIR;
}

function sortedState(column: RequestLogSortBy): SortDir | false {
  return sortBy.value === column ? sortDir.value : false;
}

function sortClearable(column: RequestLogSortBy): boolean {
  return sortBy.value === column && !isDefaultSort();
}

function ariaSort(column: RequestLogSortBy): 'ascending' | 'descending' | 'none' {
  if (sortBy.value !== column) return 'none';
  return sortDir.value === 'asc' ? 'ascending' : 'descending';
}

function onSort(column: RequestLogSortBy, dir: SortDir) {
  sortBy.value = column;
  sortDir.value = dir;
  resetResults();
}

function onClearSort() {
  sortBy.value = DEFAULT_SORT_BY;
  sortDir.value = DEFAULT_SORT_DIR;
  resetResults();
}

const columnLabels = computed((): Record<RequestLogColumnId, string> => ({
  created: t('logs.created'),
  token: t('logs.token'),
  model: t('logs.model'),
  channel: t('logs.channel'),
  inboundProtocol: t('logs.requestProtocol'),
  tokens: t('logs.tokens'),
  latency: t('logs.latencyAndSpeed'),
  cache: t('logs.cache'),
  cacheHit: t('logs.cacheHitRate'),
  cost: t('logs.cost'),
  billing: t('logs.billingDetail'),
  body: t('logs.bodyDetail'),
}));

const activeLogIds = computed(() => new Set(windows.value.map((win) => win.payload.entry.id)));

const settledOptions = computed(() => [
  { value: 'true', label: t('logs.settledYes') },
  { value: 'false', label: t('logs.settledNo') },
]);

const protocolOptions = computed(() =>
  PROTOCOLS.map((value) => ({
    value,
    label: t(`protocol.${value}`),
  })),
);

const discountOptions = computed(() => {
  const counts = new Map<number, number>();
  for (const item of items.value) {
    counts.set(item.discount_bp, (counts.get(item.discount_bp) ?? 0) + 1);
  }
  return Array.from(counts.entries())
    .sort(([a], [b]) => a - b)
    .map(([bp, count]) => ({
      value: String(bp),
      label: formatDiscountBp(bp),
      count,
    }));
});

const refreshOptions = computed(() => [
  { value: '0', label: t('logs.autoRefreshOff') },
  { value: '5', label: t('logs.autoRefreshSeconds', { seconds: 5 }) },
  { value: '10', label: t('logs.autoRefreshSeconds', { seconds: 10 }) },
  { value: '30', label: t('logs.autoRefreshSeconds', { seconds: 30 }) },
]);

// 协议映射只需要「渠道名 → 协议」，故走名录投影；完整定义是 root-only，
// 用它会让 admin 吃 403。普通用户连名录也无权读，此时 map 保持 null。
const { channels, channelsKnown } = useChannelDirectory();

/**
 * 渠道表未到手时返回 null，让详情只显示入站协议。
 *
 * 不能退化成空 Map：那会让 `resolveOutboundProtocol` 对每一行都判 `unknown`，
 * 于是「我看不到渠道表」被显示成「这条渠道不在表里」。
 */
const channelProtocolMap = computed((): Map<string, string> | null => {
  if (!channelsKnown.value) {
    return null;
  }
  return new Map<string, string>(channels.value.map((channel) => [channel.name, channel.protocol]));
});

watch(appliedSettled, resetResults);
watch(appliedDiscountBp, resetResults);
watch(appliedProtocols, resetResults);
watch([appliedModel, appliedChannel, appliedTokenName], resetResults);

function buildQuery(): LogQuery {
  const query: LogQuery = {
    page: page.value,
    page_size: pageSize.value,
  };
  const keyword = appliedKeyword.value.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (appliedModel.value) {
    query.model = appliedModel.value;
  }
  if (appliedChannel.value) {
    query.channel = appliedChannel.value;
  }
  if (appliedTokenName.value) {
    query.token_name = appliedTokenName.value;
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
  if (appliedDiscountBp.value.length === 1) {
    query.discount_bp = Number(appliedDiscountBp.value[0]);
  }
  if (appliedProtocols.value.length > 0) {
    query.inbound_protocol = appliedProtocols.value;
  }
  query.sort_by = sortBy.value;
  query.sort_dir = sortDir.value;
  return query;
}

const refetchInterval = computed(() => {
  const s = Number.parseInt(autoRefreshSeconds.value, 10);
  return s > 0 ? s * 1000 : false;
});

const logsQuery = useQuery({
  queryKey: [
    'logs',
    page,
    pageSize,
    appliedKeyword,
    appliedFrom,
    appliedTo,
    appliedSettled,
    appliedDiscountBp,
    appliedProtocols,
    appliedModel,
    appliedChannel,
    appliedTokenName,
    sortBy,
    sortDir,
  ],
  queryFn: () => apiClient.queryLogs(buildQuery()),
  refetchInterval,
});

const closeMutation = useMutation({
  mutationFn: ({ id, action }: { id: number; action: 'settle' | 'waive' }) =>
    action === 'settle' ? apiClient.settleLog(id) : apiClient.waiveLog(id),
  onSuccess: async (updatedEntry, vars) => {
    success(vars.action === 'settle' ? t('logs.settleSuccess') : t('logs.waiveSuccess'));
    const nextDetails = new Map(details.value);
    nextDetails.set(vars.id, updatedEntry);
    details.value = nextDetails;
    for (const win of windows.value) {
      if (win.payload.entry.id === vars.id) {
        win.payload.entry = { ...win.payload.entry, settled: true };
      }
    }
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
  appliedDiscountBp.value = [];
  appliedProtocols.value = [];
  appliedModel.value = null;
  appliedChannel.value = null;
  appliedTokenName.value = null;
  clearBaseFilters();
}

async function loadDetail(id: number) {
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

function openBillingWindow(event: MouseEvent, entry: LogEntry) {
  const existing = windows.value.find(
    (win) => win.payload.type === 'billing' && win.payload.entry.id === entry.id,
  );
  if (existing) {
    bringToFront(existing.id);
  } else {
    openWindow(anchorFromEvent(event), { type: 'billing', entry });
  }
}

function openBodyWindow(event: MouseEvent, entry: LogEntry) {
  const existing = windows.value.find(
    (win) => win.payload.type === 'body' && win.payload.entry.id === entry.id,
  );
  if (existing) {
    bringToFront(existing.id);
  } else {
    openWindow(anchorFromEvent(event), { type: 'body', entry });
  }
  if (!details.value.has(entry.id)) {
    void loadDetail(entry.id);
  }
}

function clearKeyword() {
  draftKeyword.value = '';
  appliedKeyword.value = '';
}

function onFilterModel(model: string) {
  appliedModel.value = model;
  clearKeyword();
}

function onFilterChannel(channel: string) {
  appliedChannel.value = channel;
  clearKeyword();
}

function onFilterToken(tokenName: string) {
  appliedTokenName.value = tokenName;
  clearKeyword();
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
          <FacetedFilter
            v-if="discountOptions.length > 0"
            v-model="appliedDiscountBp"
            :title="t('logs.discountFilter')"
            :options="discountOptions"
            test-id="logs-discount-filter"
          />
          <FacetedFilter
            v-model="appliedProtocols"
            :title="t('logs.protocolFilter')"
            :options="protocolOptions"
            test-id="logs-protocol-filter"
            menu-class="faceted-filter-menu-wide"
          />
          <button
            v-if="appliedModel"
            type="button"
            class="filter-btn"
            data-testid="logs-exact-model"
            :title="t('logs.clearColumnFilter')"
            @click="appliedModel = null"
          >
            {{ t('logs.model') }}
            <span class="faceted-filter-sep" aria-hidden="true" />
            <span class="badge badge-neutral rounded-sm px-1 font-normal">{{ appliedModel }}</span>
            <UiIcon name="close" :size="12" />
          </button>
          <button
            v-if="appliedChannel"
            type="button"
            class="filter-btn"
            data-testid="logs-exact-channel"
            :title="t('logs.clearColumnFilter')"
            @click="appliedChannel = null"
          >
            {{ t('logs.channel') }}
            <span class="faceted-filter-sep" aria-hidden="true" />
            <span class="badge badge-neutral rounded-sm px-1 font-normal">{{
              appliedChannel
            }}</span>
            <UiIcon name="close" :size="12" />
          </button>
          <button
            v-if="appliedTokenName"
            type="button"
            class="filter-btn"
            data-testid="logs-exact-token"
            :title="t('logs.clearColumnFilter')"
            @click="appliedTokenName = null"
          >
            {{ t('logs.token') }}
            <span class="faceted-filter-sep" aria-hidden="true" />
            <span class="badge badge-neutral rounded-sm px-1 font-normal">{{
              appliedTokenName
            }}</span>
            <UiIcon name="close" :size="12" />
          </button>
          <p
            v-if="unsettledTotal > 0"
            class="rounded border border-amber-500/20 bg-amber-500/10 px-2 py-1 text-xs font-semibold text-amber-600 dark:text-amber-400"
            data-testid="logs-unsettled-total"
          >
            {{ t('logs.unsettledTotal', { count: unsettledTotal }) }}
          </p>

          <template #actions>
            <div class="flex items-center gap-1.5 text-xs text-[var(--fg-muted)]">
              <UiIcon
                name="refresh-cw"
                :size="14"
                :class="{
                  'animate-spin text-[var(--seed-primary)]':
                    autoRefreshSeconds !== '0' && logsQuery.isFetching.value,
                }"
              />
              <div class="w-44 shrink-0">
                <UiSelect
                  id="logs-auto-refresh"
                  v-model="autoRefreshSeconds"
                  :options="refreshOptions"
                  :aria-label="t('logs.autoRefresh')"
                  class="text-xs"
                />
              </div>
            </div>
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
            <DataTableViewOptions
              :items="columnMenuItems"
              :labels="columnLabels"
              test-id="logs-columns"
              @toggle="setVisible"
            />
          </template>
        </DataTableToolbar>
      </template>

      <TableHeader>
        <TableRow>
          <TableHead class="w-40" :aria-sort="ariaSort('created')">
            <DataTableColumnHeader
              :label="t('logs.created')"
              :sorted="sortedState('created')"
              :clearable="sortClearable('created')"
              @sort="onSort('created', $event)"
              @clear="onClearSort"
            />
          </TableHead>
          <TableHead v-if="visible.token">{{ t('logs.token') }}</TableHead>
          <TableHead v-if="visible.model">{{ t('logs.model') }}</TableHead>
          <TableHead v-if="visible.channel">{{ t('logs.channel') }}</TableHead>
          <TableHead v-if="visible.inboundProtocol">{{ t('logs.requestProtocol') }}</TableHead>
          <TableHead v-if="visible.tokens" class="w-28" :aria-sort="ariaSort('tokens')">
            <DataTableColumnHeader
              :label="t('logs.tokens')"
              :sorted="sortedState('tokens')"
              :clearable="sortClearable('tokens')"
              @sort="onSort('tokens', $event)"
              @clear="onClearSort"
            />
          </TableHead>
          <TableHead v-if="visible.latency" class="w-36" :aria-sort="ariaSort('latency')">
            <DataTableColumnHeader
              :label="t('logs.latencyAndSpeed')"
              :sorted="sortedState('latency')"
              :clearable="sortClearable('latency')"
              @sort="onSort('latency', $event)"
              @clear="onClearSort"
            />
          </TableHead>
          <TableHead v-if="visible.cache" class="w-32" :aria-sort="ariaSort('cache')">
            <DataTableColumnHeader
              :label="t('logs.cache')"
              :sorted="sortedState('cache')"
              :clearable="sortClearable('cache')"
              @sort="onSort('cache', $event)"
              @clear="onClearSort"
            />
          </TableHead>
          <TableHead v-if="visible.cacheHit" class="w-24">
            {{ t('logs.cacheHitShort') }}
          </TableHead>
          <TableHead v-if="visible.cost" class="w-28" :aria-sort="ariaSort('cost')">
            <DataTableColumnHeader
              :label="t('logs.cost')"
              :sorted="sortedState('cost')"
              :clearable="sortClearable('cost')"
              @sort="onSort('cost', $event)"
              @clear="onClearSort"
            />
          </TableHead>
          <TableHead align="center" class="w-20">{{ t('logs.billingDetail') }}</TableHead>
          <TableHead v-if="visible.body" align="center" class="w-20">{{
            t('logs.bodyDetail')
          }}</TableHead>
        </TableRow>
      </TableHeader>

      <TableBody>
        <TableRowsSkeleton v-if="showTableSkeleton" :columns="columnCount" />
        <template v-else>
          <LogTableRow
            v-for="entry in items"
            :key="entry.id"
            :entry="entry"
            :visible="rowVisible"
            :active="activeLogIds.has(entry.id)"
            :channel-protocol-map="channelProtocolMap"
            @open-billing="openBillingWindow"
            @open-body="openBodyWindow"
            @filter-model="onFilterModel"
            @filter-channel="onFilterChannel"
            @filter-token="onFilterToken"
          />
          <TableRow v-if="items.length === 0">
            <TableCell :colspan="columnCount" class="h-24 whitespace-normal">
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

    <template v-for="(win, index) in windows" :key="win.id">
      <RequestLogDetailWindow
        v-if="win.payload.type === 'billing'"
        :entry="win.payload.entry"
        :closing="closingId === win.payload.entry.id"
        :can-settle="canSettleLogs"
        :channel-protocol-map="channelProtocolMap"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @settle="(id) => closeMutation.mutate({ id, action: 'settle' })"
        @waive="(id) => closeMutation.mutate({ id, action: 'waive' })"
        @filter-model="onFilterModel"
        @filter-channel="onFilterChannel"
        @filter-token="onFilterToken"
      />
      <RequestLogBodyWindow
        v-else-if="win.payload.type === 'body'"
        :entry="win.payload.entry"
        :detail="details.get(win.payload.entry.id) ?? null"
        :detail-loading="detailLoading.has(win.payload.entry.id)"
        :detail-error="detailErrors.get(win.payload.entry.id) ?? ''"
        :channel-protocol-map="channelProtocolMap"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @retry-detail="loadDetail(win.payload.entry.id)"
      />
    </template>
  </div>
</template>
