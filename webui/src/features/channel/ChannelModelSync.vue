<script setup lang="ts">
// 上游模型同步视图：挂载即按渠道草稿拉取上游模型列表，勾选表格 + 状态筛选，
// 「返回」把勾选结果经 emit 写回父级草稿；失败以独立浮窗就近「刷新」按钮弹出，
// 3s 自动消失（鼠标悬浮/键盘焦点暂停计时）。
import { computed, onMounted, onUnmounted, ref, useId } from 'vue';
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
import { compareModels } from '@/lib/model-list';
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

/** 状态筛选分类：按「是否勾选 × 是否在清单」收敛为三档。 */
type SyncStatusFilter = 'selected' | 'willRemove' | 'unselected';

const SYNC_STATUS_FILTERS: { value: SyncStatusFilter; labelKey: string }[] = [
  { value: 'selected', labelKey: 'channel.syncStatusSelected' },
  { value: 'unselected', labelKey: 'channel.syncStatusUnselected' },
  { value: 'willRemove', labelKey: 'channel.syncStatusWillRemove' },
];

const props = defineProps<{
  /** 进入视图时的模型清单：初始化勾选，且是「已在清单」的判定基准。 */
  models: string[];
  protocol: Protocol;
  baseUrl: string;
  apiKey: string;
  /** 草稿超时（毫秒）；非法时由父级传 null，本组件兜底缺省值。 */
  timeoutMs: number | null;
  /** 编辑器浮窗的窗口栈序号：失败浮窗叠在其上一级。 */
  stackOrder: number;
}>();

const emit = defineEmits<{
  /** 返回即提交勾选：写回模型清单草稿，保存仍由父级表单触发。 */
  back: [models: string[]];
}>();

const { t } = useI18n();
const syncSearchId = `channel-sync-search-${useId()}`;

const upstreamModels = ref<string[]>([]);
/** 勾选集：进入时以当前清单初始化。 */
const syncSelection = ref<Set<string>>(new Set(props.models));
const syncSearch = ref('');
/** 状态筛选集：空集不过滤。 */
const syncStatusFilter = ref<Set<SyncStatusFilter>>(new Set());

const syncFailure = ref<SyncFailure | null>(null);
/** 失败浮窗锚点：就近「刷新」按钮弹出，失败时取其位置。 */
const syncFailureAnchor = ref<FloatingWindowAnchor | null>(null);
const refreshBtnEl = ref<HTMLElement | null>(null);

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
    upstreamModels.value = data.models;
  },
  onError: (err) => {
    const { message, code } = extractApiError(err);
    showSyncFailure(code === undefined ? { message } : { message, code });
  },
});

onMounted(() => {
  syncMutation.mutate();
});

// --- 失败浮窗计时 ---

let failureDismissTimer: ReturnType<typeof setTimeout> | undefined;
let failureDismissRemaining = SYNC_ERROR_VISIBLE_MS;
let failureDismissStarted = 0;

function showSyncFailure(failure: SyncFailure) {
  const rect = refreshBtnEl.value?.getBoundingClientRect();
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

// --- 行与筛选 ---

interface SyncRow {
  name: string;
  /** 进入同步视图时已在模型清单中。 */
  inList: boolean;
  selected: boolean;
  status: SyncStatus;
}

/** 同步行：上游返回与进入时清单的并集，自然排序。 */
const syncRows = computed((): SyncRow[] => {
  const names = new Set([...upstreamModels.value, ...props.models]);
  return [...names].sort(compareModels).map((name) => {
    const inList = props.models.includes(name);
    const selected = syncSelection.value.has(name);
    const status: SyncStatus = inList
      ? selected
        ? 'selected'
        : 'willRemove'
      : selected
        ? 'willAdd'
        : 'unselected';
    return { name, inList, selected, status };
  });
});

/** 行归入的筛选分类：勾选即「已选择」，未勾选按是否在清单分「将取消/未选择」。 */
function rowFilterKey(row: SyncRow): SyncStatusFilter {
  if (row.selected) return 'selected';
  return row.inList ? 'willRemove' : 'unselected';
}

const filteredSyncRows = computed(() => {
  const query = syncSearch.value.trim().toLowerCase();
  return syncRows.value.filter((row) => {
    if (query !== '' && !row.name.toLowerCase().includes(query)) return false;
    if (syncStatusFilter.value.size > 0 && !syncStatusFilter.value.has(rowFilterKey(row))) {
      return false;
    }
    return true;
  });
});

const syncEmptyTitle = computed(() =>
  syncSearch.value.trim() === '' ? t('channel.syncEmpty') : t('channel.syncEmptySearch'),
);

// --- 勾选操作 ---

/** 复制勾选集、施加变更再整体赋值，保持 Set 的响应式更新。 */
function updateSelection(mutate: (next: Set<string>) => void) {
  const next = new Set(syncSelection.value);
  mutate(next);
  syncSelection.value = next;
}

function toggleSyncRow(name: string) {
  updateSelection((next) => {
    if (next.has(name)) {
      next.delete(name);
    } else {
      next.add(name);
    }
  });
}

function toggleStatusFilter(key: SyncStatusFilter) {
  const next = new Set(syncStatusFilter.value);
  if (next.has(key)) {
    next.delete(key);
  } else {
    next.add(key);
  }
  syncStatusFilter.value = next;
}

function clearStatusFilter() {
  syncStatusFilter.value = new Set();
}

/** 各筛选分类在（未做状态筛选的）行中的数量。 */
function filterCount(key: SyncStatusFilter): number {
  return syncRows.value.filter((row) => rowFilterKey(row) === key).length;
}

/** 触发按钮上的已选筛选徽章文案。 */
const activeFilterLabels = computed(() =>
  SYNC_STATUS_FILTERS.filter((option) => syncStatusFilter.value.has(option.value)).map((option) =>
    t(option.labelKey),
  ),
);

function selectAllVisible() {
  updateSelection((next) => {
    for (const row of filteredSyncRows.value) {
      next.add(row.name);
    }
  });
}

function invertVisible() {
  updateSelection((next) => {
    for (const row of filteredSyncRows.value) {
      if (next.has(row.name)) {
        next.delete(row.name);
      } else {
        next.add(row.name);
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
        next.add(row.name);
      } else {
        next.delete(row.name);
      }
    }
  });
}

function closeSync() {
  emit('back', [...syncSelection.value].sort(compareModels));
}
</script>

<template>
  <div class="card-body space-y-3" data-testid="channel-sync-view">
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
        class="min-w-0 flex-1"
        :placeholder="t('channel.syncSearchPlaceholder')"
        data-testid="channel-sync-search"
      />
      <div class="flex shrink-0 items-center gap-1.5">
        <button
          ref="refreshBtnEl"
          type="button"
          class="btn btn-sm"
          data-testid="channel-sync-refresh"
          :disabled="syncMutation.isPending.value"
          @click="syncMutation.mutate()"
        >
          {{ t('channel.syncRefresh') }}
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
              v-for="option in SYNC_STATUS_FILTERS"
              :key="option.value"
              type="button"
              class="sync-filter-option"
              :data-testid="`channel-sync-filter-${option.value}`"
              @click="toggleStatusFilter(option.value)"
            >
              <span
                class="sync-filter-box"
                :data-active="String(syncStatusFilter.has(option.value))"
                aria-hidden="true"
              >
                <UiIcon name="check" :size="10" />
              </span>
              <span class="min-w-0 flex-1">{{ t(option.labelKey) }}</span>
              <span class="sync-filter-count">{{ filterCount(option.value) }}</span>
            </button>
            <template v-if="syncStatusFilter.size > 0">
              <div class="my-1 border-t border-[var(--seed-border)]" aria-hidden="true" />
              <button
                type="button"
                class="sync-filter-option justify-center"
                data-testid="channel-sync-filter-clear"
                @click="clearStatusFilter"
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
    <DataTablePanel v-if="filteredSyncRows.length > 0">
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
            <TableHead class="w-28" align="right">{{ t('channel.syncColStatus') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRow
            v-for="(row, index) in filteredSyncRows"
            :key="row.name"
            class="cursor-pointer"
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
            <TableCell class="font-mono text-xs">{{ row.name }}</TableCell>
            <TableCell align="right">
              <span
                class="badge"
                :class="SYNC_STATUS_BADGE[row.status]"
                :data-testid="`channel-sync-status-${row.status}`"
              >
                {{ t(SYNC_STATUS_KEY[row.status]) }}
              </span>
            </TableCell>
          </TableRow>
        </TableBody>
      </Table>
    </DataTablePanel>
    <p
      v-else-if="syncMutation.isPending.value"
      class="text-fg-muted px-4 py-6 text-center text-sm"
      role="status"
    >
      {{ t('common.loading') }}
    </p>
    <EmptyState v-else :title="syncEmptyTitle" />
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
