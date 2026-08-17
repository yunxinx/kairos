<script setup lang="ts">
// 上游模型同步视图：进入后不自动请求，由用户点「同步」拉取上游模型列表；
// 勾选表格 + 别名列（常显输入、逗号分隔多个）+ 选择/别名两个维度的状态筛选，
// 「保存并返回」把勾选结果与别名映射经 emit 写回父级草稿；别名占用另一已勾选
// 主模型名或同一别名指向两个主模型时拒绝提交。失败以独立浮窗就近
// 「同步」按钮弹出，3s 自动消失（鼠标悬浮/键盘焦点暂停计时）。
import { computed, onUnmounted, ref, useId } from 'vue';
import { useMutation } from '@tanstack/vue-query';
import { PopoverContent, PopoverRoot, PopoverTrigger } from 'reka-ui';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { Protocol } from '@/api/types';
import Checkbox from '@/components/ui/Checkbox.vue';
import DataTablePanel from '@/components/ui/DataTablePanel.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import SelectCell from '@/components/ui/data-table/SelectCell.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import Table from '@/components/ui/table/Table.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import { commitSyncListing, compareModels } from '@/lib/model-list';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

/** 草稿超时非法时拉取上游模型的兜底超时，与新建渠道缺省超时一致。 */
const SYNC_TIMEOUT_FALLBACK_MS = 30_000;

/** 失败浮窗自动消失时长；鼠标悬浮/键盘焦点暂停计时。 */
const SYNC_ERROR_VISIBLE_MS = 3_000;

/** 同步失败：以独立浮窗展示。 */
interface SyncFailure {
  message: string;
  code?: string;
}

/** 同步行状态：「已在清单」与当前勾选的组合语义。 */
type SyncStatus = 'selected' | 'willRemove' | 'willAdd' | 'unselected';

const SYNC_STATUS_BADGE: Record<SyncStatus, string> = {
  selected: 'badge-success',
  willRemove: 'badge-danger',
  willAdd: 'badge-info',
  unselected: 'badge-neutral',
};

const SYNC_STATUS_KEY: Record<SyncStatus, string> = {
  selected: 'channel.syncStatusSelected',
  willRemove: 'channel.syncStatusWillRemove',
  willAdd: 'channel.syncStatusWillAdd',
  unselected: 'channel.syncStatusUnselected',
};

/** 选择状态筛选：按「是否勾选 × 是否在清单」收敛为三档。 */
type SyncSelectionFilter = 'selected' | 'willRemove' | 'unselected';

const SELECTION_FILTERS: { value: SyncSelectionFilter; labelKey: string }[] = [
  { value: 'selected', labelKey: 'channel.syncStatusSelected' },
  { value: 'unselected', labelKey: 'channel.syncStatusUnselected' },
  { value: 'willRemove', labelKey: 'channel.syncStatusWillRemove' },
];

/** 别名筛选：与选择状态正交的第二维度，按谓词匹配（一行可同时命中多项）。 */
type SyncAliasFilter = 'hasAlias' | 'noAlias' | 'aliasOnly';

const ALIAS_FILTERS: { value: SyncAliasFilter; labelKey: string }[] = [
  { value: 'hasAlias', labelKey: 'channel.syncFilterHasAlias' },
  { value: 'noAlias', labelKey: 'channel.syncFilterNoAlias' },
  { value: 'aliasOnly', labelKey: 'channel.syncFilterAliasOnly' },
];

const props = defineProps<{
  /** 进入视图时的模型清单（主模型名与别名混排）：初始化勾选与「已在清单」基准。 */
  models: string[];
  /** 进入视图时的别名映射（别名 → 主模型名）：初始化别名列草稿。 */
  aliases: Record<string, string>;
  protocol: Protocol;
  baseUrl: string;
  apiKey: string;
  /** 草稿超时（毫秒）；非法时由父级传 null，本组件兜底缺省值。 */
  timeoutMs: number | null;
  /** 编辑器浮窗的窗口栈序号：失败浮窗叠在其上一级。 */
  stackOrder: number;
}>();

const emit = defineEmits<{
  /** 保存并返回即提交：勾选模型（含别名）与别名映射一并写回草稿，保存仍由父级表单触发。 */
  back: [models: string[], aliases: Record<string, string>];
}>();

const { t } = useI18n();
const syncSearchId = `channel-sync-search-${useId()}`;

const upstreamModels = ref<string[]>([]);
/** 是否已完成过一次同步：区分「尚未同步」与「上游未返回模型」两种空态。 */
const hasSynced = ref(false);
const syncSearch = ref('');
/** 仅当真正的输入框聚焦时铺满行尾；清除按钮聚焦不算。 */
const searchFocused = ref(false);
/** 两个筛选维度各自独立，空集不过滤。 */
const selectionFilter = ref<Set<SyncSelectionFilter>>(new Set());
const aliasFilter = ref<Set<SyncAliasFilter>>(new Set());

const syncFailure = ref<SyncFailure | null>(null);
/** 失败浮窗锚点：就近「同步」按钮弹出，失败时取其位置。 */
const syncFailureAnchor = ref<FloatingWindowAnchor | null>(null);
const syncBtnEl = ref<HTMLElement | null>(null);

const syncMutation = useMutation({
  mutationFn: () =>
    apiClient.listUpstreamModels({
      protocol: props.protocol,
      base_url: props.baseUrl,
      api_key: props.apiKey,
      timeout_ms: props.timeoutMs ?? SYNC_TIMEOUT_FALLBACK_MS,
    }),
  onSuccess: (data) => {
    dismissSyncFailure();
    hasSynced.value = true;
    upstreamModels.value = data.models;
  },
  onError: (err) => {
    const { message, code } = extractApiError(err);
    showSyncFailure(code === undefined ? { message } : { message, code });
  },
});

// --- 失败浮窗计时 ---

let failureDismissTimer: ReturnType<typeof setTimeout> | undefined;
let failureDismissRemaining = SYNC_ERROR_VISIBLE_MS;
let failureDismissStarted = 0;

function showSyncFailure(failure: SyncFailure) {
  const rect = syncBtnEl.value?.getBoundingClientRect();
  syncFailureAnchor.value = rect ? { x: rect.left, y: rect.bottom } : null;
  syncFailure.value = failure;
  failureDismissRemaining = SYNC_ERROR_VISIBLE_MS;
  startFailureDismiss();
}

function dismissSyncFailure() {
  if (failureDismissTimer !== undefined) {
    clearTimeout(failureDismissTimer);
    failureDismissTimer = undefined;
  }
  syncFailure.value = null;
}

function startFailureDismiss() {
  if (failureDismissTimer !== undefined) clearTimeout(failureDismissTimer);
  failureDismissStarted = Date.now();
  failureDismissTimer = setTimeout(
    () => {
      failureDismissTimer = undefined;
      syncFailure.value = null;
    },
    Math.max(failureDismissRemaining, 0),
  );
}

/** 鼠标悬浮/焦点进入暂停自动消失计时。 */
function pauseFailureDismiss() {
  if (failureDismissTimer === undefined) return;
  clearTimeout(failureDismissTimer);
  failureDismissTimer = undefined;
  failureDismissRemaining -= Date.now() - failureDismissStarted;
}

/** 离开后从剩余时长继续计时。 */
function resumeFailureDismiss() {
  if (syncFailure.value === null || failureDismissTimer !== undefined) return;
  startFailureDismiss();
}

onUnmounted(() => {
  if (failureDismissTimer !== undefined) clearTimeout(failureDismissTimer);
});

// --- 进入时的草稿初始化 ---

/** 清单条目对应的主模型名：别名取其指向的主模型名，主模型名取自身。 */
function canonicalOf(name: string): string {
  return props.aliases[name] ?? name;
}

/** 初始别名映射反查：主模型名 → 别名列表（props 为开窗快照，视图生命周期内不变）。 */
const canonicalToInitialAliases = new Map<string, string[]>();
for (const [alias, canonical] of Object.entries(props.aliases)) {
  const list = canonicalToInitialAliases.get(canonical);
  if (list) {
    list.push(alias);
  } else {
    canonicalToInitialAliases.set(canonical, [alias]);
  }
}

/** 勾选集：清单条目（主模型名或别名）一律归属其主模型名。 */
const syncSelection = ref<Set<string>>(new Set(props.models.map(canonicalOf)));

/** 别名列草稿：主模型名 → 逗号分隔文本。 */
const aliasDrafts = ref<Record<string, string>>({});
/** 「仅别名生效」：主模型名已移出清单、别名保留；重新勾选即恢复主模型名。 */
const primaryDeleted = ref<Record<string, boolean>>({});
/** 进入时是否已在清单（主模型名或任一别名在清单中）。 */
const initialInList = new Map<string, boolean>();

for (const canonical of new Set([
  ...canonicalToInitialAliases.keys(),
  ...props.models.map(canonicalOf),
])) {
  const initialAliases = canonicalToInitialAliases.get(canonical) ?? [];
  aliasDrafts.value[canonical] = initialAliases.join(', ');
  const canonicalInList = props.models.includes(canonical);
  const aliasInList = initialAliases.some((alias) => props.models.includes(alias));
  primaryDeleted.value[canonical] = !canonicalInList && aliasInList;
  initialInList.set(canonical, canonicalInList || aliasInList);
}

/** 解析别名输入：按英文逗号拆分、裁剪、去空去重。 */
function parseAliasText(text: string): string[] {
  const seen = new Set<string>();
  const result: string[] = [];
  for (const part of text.split(',')) {
    const alias = part.trim();
    if (alias === '' || seen.has(alias)) continue;
    seen.add(alias);
    result.push(alias);
  }
  return result;
}

function setAliasDraft(name: string, value: string) {
  aliasDrafts.value[name] = value;
}

// --- 行与筛选 ---

interface SyncRow {
  /** 主模型名（行键）。 */
  name: string;
  aliases: string[];
  selected: boolean;
  /** 主模型名已移出清单但别名保留，且仍处于勾选态。 */
  aliasOnly: boolean;
  /** 进入视图时已在清单。 */
  inList: boolean;
  status: SyncStatus;
}

/** 同步行：上游返回、清单归属的主模型名与初始别名涉及的主模型名三者并集，自然排序。 */
const syncRows = computed((): SyncRow[] => {
  const names = new Set<string>(upstreamModels.value);
  for (const model of props.models) names.add(canonicalOf(model));
  for (const canonical of canonicalToInitialAliases.keys()) names.add(canonical);
  return [...names].sort(compareModels).map((name) => {
    const aliases = parseAliasText(aliasDrafts.value[name] ?? '');
    const selected = syncSelection.value.has(name);
    const aliasOnly = primaryDeleted.value[name] === true && selected && aliases.length > 0;
    const inList = initialInList.get(name) ?? false;
    const status: SyncStatus = inList
      ? selected
        ? 'selected'
        : 'willRemove'
      : selected
        ? 'willAdd'
        : 'unselected';
    return { name, aliases, selected, aliasOnly, inList, status };
  });
});

/** 行归入的选择筛选分类：勾选即「已选择」，未勾选按是否在清单分「将取消/未选择」。 */
function rowSelectionKey(row: SyncRow): SyncSelectionFilter {
  if (row.selected) return 'selected';
  return row.inList ? 'willRemove' : 'unselected';
}

function aliasMatches(row: SyncRow, key: SyncAliasFilter): boolean {
  switch (key) {
    case 'hasAlias':
      return row.aliases.length > 0;
    case 'noAlias':
      return row.aliases.length === 0;
    case 'aliasOnly':
      return row.aliasOnly;
  }
}

const filteredSyncRows = computed(() => {
  const query = syncSearch.value.trim().toLowerCase();
  return syncRows.value.filter((row) => {
    if (
      query !== '' &&
      !row.name.toLowerCase().includes(query) &&
      !row.aliases.some((alias) => alias.toLowerCase().includes(query))
    ) {
      return false;
    }
    if (selectionFilter.value.size > 0 && !selectionFilter.value.has(rowSelectionKey(row))) {
      return false;
    }
    if (
      aliasFilter.value.size > 0 &&
      !ALIAS_FILTERS.some(
        (option) => aliasFilter.value.has(option.value) && aliasMatches(row, option.value),
      )
    ) {
      return false;
    }
    return true;
  });
});

const syncEmptyTitle = computed(() => {
  if (syncRows.value.length > 0) return t('channel.syncEmptySearch');
  if (!hasSynced.value) return t('channel.syncNotSynced');
  return syncSearch.value.trim() === '' ? t('channel.syncEmpty') : t('channel.syncEmptySearch');
});

// --- 勾选操作 ---

/** 复制勾选集、施加变更再整体赋值，保持 Set 的响应式更新。 */
function updateSelection(mutate: (next: Set<string>) => void) {
  const next = new Set(syncSelection.value);
  mutate(next);
  syncSelection.value = next;
}

/** 勾选一行：重新勾上「仅别名生效」的行时恢复其主模型名。 */
function selectName(next: Set<string>, name: string) {
  next.add(name);
  if (primaryDeleted.value[name] === true) primaryDeleted.value[name] = false;
}

function toggleSyncRow(name: string) {
  updateSelection((next) => {
    if (next.has(name)) {
      next.delete(name);
    } else {
      selectName(next, name);
    }
  });
}

function toggleSelectionFilter(key: SyncSelectionFilter) {
  const next = new Set(selectionFilter.value);
  if (next.has(key)) {
    next.delete(key);
  } else {
    next.add(key);
  }
  selectionFilter.value = next;
}

function toggleAliasFilter(key: SyncAliasFilter) {
  const next = new Set(aliasFilter.value);
  if (next.has(key)) {
    next.delete(key);
  } else {
    next.add(key);
  }
  aliasFilter.value = next;
}

function clearFilters() {
  selectionFilter.value = new Set();
  aliasFilter.value = new Set();
}

const hasActiveFilter = computed(
  () => selectionFilter.value.size > 0 || aliasFilter.value.size > 0,
);

/** 各筛选分类在（未做状态筛选的）行中的数量。 */
function selectionFilterCount(key: SyncSelectionFilter): number {
  return syncRows.value.filter((row) => rowSelectionKey(row) === key).length;
}

function aliasFilterCount(key: SyncAliasFilter): number {
  return syncRows.value.filter((row) => aliasMatches(row, key)).length;
}

/** 触发按钮上的已选筛选徽章文案。 */
const activeFilterLabels = computed(() => [
  ...SELECTION_FILTERS.filter((option) => selectionFilter.value.has(option.value)).map((option) =>
    t(option.labelKey),
  ),
  ...ALIAS_FILTERS.filter((option) => aliasFilter.value.has(option.value)).map((option) =>
    t(option.labelKey),
  ),
]);

function selectAllVisible() {
  updateSelection((next) => {
    for (const row of filteredSyncRows.value) {
      selectName(next, row.name);
    }
  });
}

function invertVisible() {
  updateSelection((next) => {
    for (const row of filteredSyncRows.value) {
      if (next.has(row.name)) {
        next.delete(row.name);
      } else {
        selectName(next, row.name);
      }
    }
  });
}

/** 表头全选框：可见行全部勾选；部分勾选为半选。 */
const allVisibleSelected = computed(
  () => filteredSyncRows.value.length > 0 && filteredSyncRows.value.every((row) => row.selected),
);
const someVisibleSelected = computed(() => filteredSyncRows.value.some((row) => row.selected));

function toggleAllVisible() {
  const selectAll = !allVisibleSelected.value;
  updateSelection((next) => {
    for (const row of filteredSyncRows.value) {
      if (selectAll) {
        selectName(next, row.name);
      } else {
        next.delete(row.name);
      }
    }
  });
}

/** 勾选与别名草稿收成清单；占用已勾选主模型名或同一别名指向两个主模型时拒绝。 */
const listingCommit = computed(() => commitSyncListing(syncRows.value));
const listingConflict = computed(() =>
  listingCommit.value.ok ? null : listingCommit.value.conflict,
);
const listingConflictMessage = computed(() => {
  const conflict = listingConflict.value;
  if (conflict === null) return '';
  if (conflict.kind === 'occupies_selected') {
    return t('channel.syncAliasOccupiesSelected', {
      alias: conflict.alias,
      owner: conflict.owner,
      occupied: conflict.occupied,
    });
  }
  return t('channel.syncAliasClaimedTwice', {
    alias: conflict.alias,
    first: conflict.first,
    second: conflict.second,
  });
});

function rowAliasConflicts(row: SyncRow): boolean {
  const conflict = listingConflict.value;
  return conflict !== null && row.aliases.includes(conflict.alias);
}

/** 保存并返回即提交：勾选行产出主模型名与别名入清单；「仅别名生效」行只产出别名。 */
function closeSync() {
  const committed = listingCommit.value;
  if (!committed.ok) return;
  emit('back', committed.models, committed.aliases);
}
</script>

<template>
  <div class="card-body space-y-3" data-testid="channel-sync-view">
    <!-- 搜索框聚焦时动作按钮让位，输入框铺到行尾；失焦后恢复。 -->
    <div class="flex items-center gap-2">
      <button
        type="button"
        class="btn btn-sm btn-compact shrink-0"
        data-testid="channel-sync-back"
        @click="closeSync"
      >
        <UiIcon name="chevron-left" :size="14" />
        {{ t('channel.syncBack') }}
      </button>
      <SearchInput
        :id="syncSearchId"
        v-model="syncSearch"
        class="search-input-sm min-w-0 flex-1"
        :placeholder="t('channel.syncSearchPlaceholder')"
        data-testid="channel-sync-search"
        @focus="searchFocused = true"
        @blur="searchFocused = false"
      />
      <div class="flex shrink-0 items-center gap-1.5" :class="searchFocused && 'hidden'">
        <button
          ref="syncBtnEl"
          type="button"
          class="btn btn-sm"
          data-testid="channel-sync-run"
          :disabled="syncMutation.isPending.value"
          @click="syncMutation.mutate()"
        >
          {{ t('channel.syncAction') }}
        </button>
        <PopoverRoot>
          <PopoverTrigger
            class="filter-btn"
            data-testid="channel-sync-filter"
            :aria-label="t('channel.syncFilterTitle')"
          >
            <template v-if="activeFilterLabels.length === 0">
              <UiIcon name="plus-circle" :size="14" />
              {{ t('channel.syncFilterTitle') }}
            </template>
            <template v-else-if="activeFilterLabels.length <= 2">
              <span
                v-for="label in activeFilterLabels"
                :key="label"
                class="badge badge-neutral rounded-sm px-1 font-normal"
              >
                {{ label }}
              </span>
            </template>
            <span v-else class="badge badge-neutral rounded-sm px-1 font-normal">
              {{ t('common.selectedCount', { count: activeFilterLabels.length }) }}
            </span>
          </PopoverTrigger>
          <PopoverContent
            align="start"
            :side-offset="4"
            class="z-10 w-50 rounded-md border border-[var(--seed-border)] bg-[var(--seed-surface)] p-1 shadow-md"
            data-testid="channel-sync-filter-menu"
          >
            <button
              v-for="option in SELECTION_FILTERS"
              :key="option.value"
              type="button"
              class="sync-filter-option"
              :data-testid="`channel-sync-filter-${option.value}`"
              @click="toggleSelectionFilter(option.value)"
            >
              <span
                class="sync-filter-box"
                :data-active="String(selectionFilter.has(option.value))"
                aria-hidden="true"
              >
                <UiIcon name="check" :size="10" />
              </span>
              <span class="min-w-0 flex-1">{{ t(option.labelKey) }}</span>
              <span class="sync-filter-count">{{ selectionFilterCount(option.value) }}</span>
            </button>
            <div class="my-1 border-t border-[var(--seed-border)]" aria-hidden="true" />
            <button
              v-for="option in ALIAS_FILTERS"
              :key="option.value"
              type="button"
              class="sync-filter-option"
              :data-testid="`channel-sync-filter-${option.value}`"
              @click="toggleAliasFilter(option.value)"
            >
              <span
                class="sync-filter-box"
                :data-active="String(aliasFilter.has(option.value))"
                aria-hidden="true"
              >
                <UiIcon name="check" :size="10" />
              </span>
              <span class="min-w-0 flex-1">{{ t(option.labelKey) }}</span>
              <span class="sync-filter-count">{{ aliasFilterCount(option.value) }}</span>
            </button>
            <template v-if="hasActiveFilter">
              <div class="my-1 border-t border-[var(--seed-border)]" aria-hidden="true" />
              <button
                type="button"
                class="sync-filter-option justify-center"
                data-testid="channel-sync-filter-clear"
                @click="clearFilters"
              >
                {{ t('channel.syncFilterClear') }}
              </button>
            </template>
          </PopoverContent>
        </PopoverRoot>
        <button
          type="button"
          class="btn btn-sm"
          data-testid="channel-sync-select-all"
          :disabled="filteredSyncRows.length === 0"
          @click="selectAllVisible"
        >
          {{ t('channel.syncSelectAll') }}
        </button>
        <button
          type="button"
          class="btn btn-sm"
          data-testid="channel-sync-invert"
          :disabled="filteredSyncRows.length === 0"
          @click="invertVisible"
        >
          {{ t('channel.syncInvert') }}
        </button>
      </div>
    </div>
    <p
      v-if="listingConflict !== null"
      class="text-danger text-sm"
      role="alert"
      data-testid="channel-sync-alias-conflict"
    >
      {{ listingConflictMessage }}
    </p>
    <DataTablePanel>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead class="w-10">
              <div class="flex items-center justify-center">
                <Checkbox
                  :model-value="allVisibleSelected"
                  :indeterminate="someVisibleSelected && !allVisibleSelected"
                  data-testid="channel-sync-select-all-head"
                  :aria-label="t('common.selectAll')"
                  @update:model-value="toggleAllVisible"
                />
              </div>
            </TableHead>
            <TableHead class="w-12">{{ t('channel.syncColIndex') }}</TableHead>
            <TableHead>{{ t('channel.syncColModel') }}</TableHead>
            <TableHead class="sync-col-alias">{{ t('channel.syncColAlias') }}</TableHead>
            <TableHead class="sync-col-status" align="right">{{
              t('channel.syncColStatus')
            }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRow
            v-for="(row, index) in filteredSyncRows"
            :key="row.name"
            class="cursor-pointer"
            :class="row.aliasOnly && 'sync-row-alias-only'"
            :data-state="row.selected ? 'selected' : undefined"
            data-testid="channel-sync-row"
            :data-model="row.name"
            @click="toggleSyncRow(row.name)"
          >
            <SelectCell
              :checked="row.selected"
              test-id="channel-sync-checkbox"
              @toggle="toggleSyncRow(row.name)"
              @click.stop
            />
            <TableCell class="text-fg-muted font-mono text-xs">{{ index + 1 }}</TableCell>
            <TableCell class="font-mono text-xs">
              <span :class="row.aliasOnly ? 'model-name-deleted' : undefined">{{ row.name }}</span>
            </TableCell>
            <TableCell class="sync-col-alias" @click.stop>
              <input
                type="text"
                class="sync-alias-input"
                :class="rowAliasConflicts(row) && 'sync-alias-input-invalid'"
                :value="aliasDrafts[row.name] ?? ''"
                :placeholder="t('channel.syncAliasPlaceholder')"
                data-testid="channel-sync-alias-input"
                :aria-invalid="rowAliasConflicts(row) ? 'true' : undefined"
                :aria-label="`${t('channel.syncColAlias')}: ${row.name}`"
                @input="setAliasDraft(row.name, ($event.target as HTMLInputElement).value)"
              />
            </TableCell>
            <TableCell class="sync-col-status" align="right">
              <span
                class="badge"
                :class="SYNC_STATUS_BADGE[row.status]"
                :data-testid="`channel-sync-status-${row.status}`"
              >
                {{ t(SYNC_STATUS_KEY[row.status]) }}
              </span>
            </TableCell>
          </TableRow>
          <TableRow v-if="filteredSyncRows.length === 0">
            <TableCell :colspan="5" class="h-24 whitespace-normal">
              <p
                v-if="syncMutation.isPending.value"
                class="text-fg-muted px-4 py-6 text-center text-sm"
                role="status"
              >
                {{ t('common.loading') }}
              </p>
              <EmptyState v-else :title="syncEmptyTitle" />
            </TableCell>
          </TableRow>
        </TableBody>
      </Table>
    </DataTablePanel>
  </div>

  <!-- 失败浮窗：不响应 Esc（topmost=false），避免与编辑器浮窗同时关闭。 -->
  <FloatingWindow
    v-if="syncFailure"
    :title="t('channel.syncErrorTitle')"
    :anchor="syncFailureAnchor"
    :stack-order="stackOrder + 1"
    :topmost="false"
    @close="dismissSyncFailure"
    @mouseenter="pauseFailureDismiss"
    @mouseleave="resumeFailureDismiss"
    @focusin="pauseFailureDismiss"
    @focusout="resumeFailureDismiss"
  >
    <div class="card-body space-y-2" data-testid="channel-sync-error">
      <p class="text-danger text-sm">{{ syncFailure.message }}</p>
      <div v-if="syncFailure.code" class="text-fg-muted space-y-1 text-xs">
        <p class="font-medium">{{ t('channel.syncErrorDetail') }}</p>
        <p class="font-mono">{{ t('channel.syncErrorCode') }}: {{ syncFailure.code }}</p>
        <p class="font-mono break-all">{{ t('channel.syncErrorTarget') }}: {{ baseUrl }}</p>
      </div>
    </div>
  </FloatingWindow>
</template>
