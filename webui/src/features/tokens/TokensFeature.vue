<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import { loadTokenRows, type TokenRow } from '@/api/token-rows';
import PageHeader from '@/app/layout/PageHeader.vue';
import Checkbox from '@/components/ui/Checkbox.vue';
import ConfirmWindow from '@/components/ui/ConfirmWindow.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FacetedFilter from '@/components/ui/FacetedFilter.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import InlineError from '@/components/ui/InlineError.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import DataTable from '@/components/ui/data-table/DataTable.vue';
import DataTableBulkBar from '@/components/ui/data-table/DataTableBulkBar.vue';
import DataTableMenuItem from '@/components/ui/data-table/DataTableMenuItem.vue';
import DataTableRowActions from '@/components/ui/data-table/DataTableRowActions.vue';
import DataTableToolbar from '@/components/ui/data-table/DataTableToolbar.vue';
import SelectCell from '@/components/ui/data-table/SelectCell.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import { useBulkDelete, type BulkDeletePayload } from '@/composables/useBulkDelete';
import { useRowSelection } from '@/composables/useRowSelection';
import { useWindowStack } from '@/composables/useWindowStack';
import { useToast } from '@/composables/useToast';
import TokenEditorWindow from '@/features/tokens/TokenEditorWindow.vue';
import {
  formatUnixMillis,
  formatUsdFixed2,
  formatUsdMicros,
  maskTokenKey,
  relativeTimeParts,
} from '@/lib/format';
import { groupDisplayName } from '@/lib/visible-models';
import { anchorFromEvent, type FloatingWindowAnchor } from '@/lib/window-anchor';

type TokenWindowPayload =
  | { kind: 'editor'; token: TokenRow | null }
  | { kind: 'delete'; token: TokenRow }
  | BulkDeletePayload;

/** 额度剩余比例分档：>50% 绿、20%–50% 黄、≤20% 红。 */
const REMAINING_WARN_RATIO = 0.5;
const REMAINING_DANGER_RATIO = 0.2;
/** 相对时间展示的刷新间隔（毫秒）。 */
const RELATIVE_TIME_TICK_MS = 30_000;
/** 复制成功后对号停留时长，与日志 body 复制反馈对齐。 */
const COPY_FEEDBACK_MS = 2_000;

const { t, locale } = useI18n();
const { error } = useToast();
const queryClient = useQueryClient();

const searchText = ref('');
const statusFilter = ref<string[]>([]);
const copiedKey = ref<string | null>(null);
let copiedTimer: ReturnType<typeof setTimeout> | undefined;
const pendingAnchor = ref<FloatingWindowAnchor | null>(null);

function takePendingAnchor(): FloatingWindowAnchor | null {
  const anchor = pendingAnchor.value;
  pendingAnchor.value = null;
  return anchor;
}

const {
  windows,
  topmostId,
  open: openWindow,
  close: closeWindow,
  setDirty,
  bringToFront,
} = useWindowStack<TokenWindowPayload>();

const tokensQuery = useQuery({
  queryKey: ['tokens'],
  queryFn: loadTokenRows,
});

const tokens = computed(() => tokensQuery.data.value ?? []);
const showTableSkeleton = computed(() => tokensQuery.isPending.value && !tokensQuery.data.value);

const statusOptions = computed(() => {
  const enabled = tokens.value.filter((token) => token.enabled).length;
  return [
    { value: 'enabled', label: t('tokens.statusEnabled'), count: enabled },
    { value: 'disabled', label: t('tokens.statusDisabled'), count: tokens.value.length - enabled },
  ];
});

const filteredTokens = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  const statuses = new Set(statusFilter.value);
  return tokens.value.filter((token) => {
    if (statuses.size > 0) {
      const flag = token.enabled ? 'enabled' : 'disabled';
      if (!statuses.has(flag)) return false;
    }
    if (!q) return true;
    return token.name.toLowerCase().includes(q) || token.token_key.toLowerCase().includes(q);
  });
});

// 行选择：全选只作用于当前可见行；被筛掉的已选行保留选择但不计入全选。
const selection = useRowSelection<number>();

const allVisibleSelected = computed({
  get: () =>
    filteredTokens.value.length > 0 &&
    filteredTokens.value.every((token) => selection.isSelected(token.id)),
  set: (value) =>
    selection.setMany(
      filteredTokens.value.map((token) => token.id),
      value,
    ),
});

const someVisibleSelected = computed(() =>
  filteredTokens.value.some((token) => selection.isSelected(token.id)),
);

// 删除或刷新后列表键变化，剔除幽灵选择。
watch(tokens, (rows) => selection.prune(rows.map((row) => row.id)));

const bulkDelete = useBulkDelete<number>({
  selection,
  windowStack: { windows, close: closeWindow },
  queryKey: ['tokens'],
  deleteMany: (ids) => apiClient.deleteTokens(ids),
});

// 相对时间随时间推移刷新：定时推进 now，避免「3 秒前」长期停留。
const now = ref(Date.now());
let relativeTimer: ReturnType<typeof setInterval> | undefined;
onMounted(() => {
  relativeTimer = setInterval(() => {
    now.value = Date.now();
  }, RELATIVE_TIME_TICK_MS);
});
onUnmounted(() => {
  if (relativeTimer !== undefined) clearInterval(relativeTimer);
  if (copiedTimer !== undefined) clearTimeout(copiedTimer);
});

async function copyKey(key: string) {
  try {
    await navigator.clipboard.writeText(key);
    copiedKey.value = key;
    if (copiedTimer !== undefined) clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => {
      if (copiedKey.value === key) copiedKey.value = null;
    }, COPY_FEEDBACK_MS);
  } catch {
    error(t('common.copyFailedTokenKey'));
  }
}

/**
 * 该令牌累计结算占其可用余额上限的比例；未设上限返回 null（不画进度条）。
 *
 * 分母是令牌自己的 `limit_usd_micros`，不是钱包：钱包是所属用户的共享余额
 * （ADR-0008），拿它跟单把令牌的 settled 相比会让同一钱包下每把令牌各显示一个
 * 无意义的百分比。
 */
function quotaRatio(token: TokenRow): number | null {
  const limit = token.limit_usd_micros;
  if (limit === null || limit <= 0) return null;
  return Math.min(1, Math.max(0, token.settled_usd_micros / limit));
}

/** 用量越接近上限越危险（与「剩余越少越危险」方向相反）。 */
function quotaColorClass(ratio: number): string {
  if (ratio >= 1 - REMAINING_DANGER_RATIO) return 'bg-[var(--danger)]';
  if (ratio >= 1 - REMAINING_WARN_RATIO) return 'bg-[var(--warn)]';
  return 'bg-[var(--success)]';
}

function quotaLabel(token: TokenRow): string {
  const settled = formatUsdMicros(token.settled_usd_micros);
  if (token.limit_usd_micros === null) {
    return t('tokens.quotaUnlimitedUsage', { settled });
  }
  return t('tokens.quotaUsage', {
    settled,
    limit: formatUsdMicros(token.limit_usd_micros),
  });
}

function formatRelative(millis: number): string {
  const parts = relativeTimeParts(now.value - millis);
  switch (parts.kind) {
    case 'seconds':
      return t('time.secondsAgo', { seconds: parts.seconds });
    case 'minutesSeconds':
      return t('time.minutesSecondsAgo', { minutes: parts.minutes, seconds: parts.seconds });
    case 'hoursMinutes':
      return t('time.hoursMinutesAgo', { hours: parts.hours, minutes: parts.minutes });
    case 'daysHours':
      return t('time.daysHoursAgo', { days: parts.days, hours: parts.hours });
    case 'monthsDays':
      return t('time.monthsDaysAgo', { months: parts.months, days: parts.days });
    case 'yearsMonthsDays':
      return t('time.yearsMonthsDaysAgo', {
        years: parts.years,
        months: parts.months,
        days: parts.days,
      });
  }
}

function formatCreated(millis: number): string {
  return formatUnixMillis(millis, locale.value);
}

const deleteErrors = ref<Record<number, string>>({});

const deleteMutation = useMutation({
  mutationFn: (id: number) => apiClient.deleteToken(id),
  onSuccess: async (_data, id) => {
    const entry = windows.value.find(
      (item) => item.payload.kind === 'delete' && item.payload.token.id === id,
    );
    if (entry) closeWindow(entry.id);
    await queryClient.invalidateQueries({ queryKey: ['tokens'] });
  },
  onError: (err, id) => {
    const message = extractApiError(err).message;
    error(message);
    const entry = windows.value.find(
      (item) => item.payload.kind === 'delete' && item.payload.token.id === id,
    );
    if (entry) deleteErrors.value[entry.id] = message;
  },
});

const deletingId = computed(() =>
  deleteMutation.isPending.value ? (deleteMutation.variables.value ?? null) : null,
);

// 启用/禁用：只提交状态字段，成功后重取列表。
const toggleMutation = useMutation({
  mutationFn: (token: TokenRow) => apiClient.setTokenEnabled(token.id, !token.enabled),
  onSuccess: async () => {
    await queryClient.invalidateQueries({ queryKey: ['tokens'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

const togglingKey = computed(() =>
  toggleMutation.isPending.value ? (toggleMutation.variables.value?.token_key ?? null) : null,
);

function openCreate(event: Event) {
  openWindow(anchorFromEvent(event), { kind: 'editor', token: null });
}

function openEdit(token: TokenRow) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'editor' && entry.payload.token?.id === token.id,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'editor', token });
}

function openDelete(token: TokenRow) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'delete' && entry.payload.token.id === token.id,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  const entry = openWindow(takePendingAnchor(), { kind: 'delete', token });
  if (entry) deleteErrors.value[entry.id] = '';
}

function openBulkDelete() {
  const existing = windows.value.find((entry) => entry.payload.kind === 'bulk-delete');
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'bulk-delete' });
}
</script>

<template>
  <div class="flex flex-col">
    <PageHeader :title="t('nav.tokens')" />

    <InlineError
      v-if="tokensQuery.isError.value && !tokensQuery.data.value"
      :message="extractApiError(tokensQuery.error.value).message"
      @retry="() => tokensQuery.refetch()"
    />

    <div v-else class="flex flex-col">
      <DataTable :busy="showTableSkeleton">
        <template #toolbar>
          <DataTableToolbar>
            <SearchInput
              id="tokens-search"
              v-model="searchText"
              class="max-w-sm"
              data-testid="tokens-search"
              :placeholder="t('tokens.search')"
              :aria-label="t('tokens.search')"
            />
            <FacetedFilter
              v-model="statusFilter"
              :title="t('tokens.status')"
              :options="statusOptions"
              test-id="tokens-status-filter"
            />
            <template #actions>
              <button
                type="button"
                class="btn btn-primary"
                data-testid="create-token"
                @click="openCreate"
              >
                {{ t('tokens.create') }}
              </button>
            </template>
          </DataTableToolbar>
        </template>
        <TableHeader>
          <TableRow>
            <TableHead class="w-10">
              <div class="flex items-center justify-center">
                <Checkbox
                  v-model="allVisibleSelected"
                  :indeterminate="someVisibleSelected && !allVisibleSelected"
                  data-testid="tokens-select-all"
                  :aria-label="t('common.selectAll')"
                />
              </div>
            </TableHead>
            <TableHead class="min-w-44">{{ t('tokens.name') }}</TableHead>
            <TableHead>{{ t('tokens.modelGroup') }}</TableHead>
            <TableHead class="min-w-56">{{ t('tokens.key') }}</TableHead>
            <TableHead>{{ t('tokens.quota') }}</TableHead>
            <TableHead align="center">{{ t('tokens.rateLimitRpm') }}</TableHead>
            <TableHead align="center">{{ t('tokens.status') }}</TableHead>
            <TableHead align="center">{{ t('tokens.createdAt') }}</TableHead>
            <TableHead align="center">{{ t('tokens.lastUsedAt') }}</TableHead>
            <TableHead align="center">{{ t('common.actions') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" has-select-column :columns="10" />
          <template v-else>
            <TableRow
              v-for="token in filteredTokens"
              :key="token.token_key"
              data-testid="token-row"
              :data-token-key="token.token_key"
              :data-state="selection.isSelected(token.id) ? 'selected' : undefined"
            >
              <SelectCell
                :checked="selection.isSelected(token.id)"
                test-id="token-select"
                @toggle="selection.toggle(token.id)"
              />
              <TableCell class="font-medium">{{ token.name }}</TableCell>
              <TableCell class="font-mono text-sm" data-testid="token-model-group">
                <span class="inline-flex items-center gap-1">
                  {{ groupDisplayName(token.model_group, t('models.ungrouped')) }}
                  <span
                    v-if="!token.group_usable"
                    class="badge badge-danger"
                    data-testid="token-group-unusable"
                    :title="t('tokens.groupUnusableHint')"
                  >
                    {{ t('tokens.groupUnusable') }}
                  </span>
                </span>
              </TableCell>
              <TableCell>
                <span class="inline-flex items-center gap-1">
                  <code class="code-chip rounded px-2 py-0.5 font-mono text-xs">
                    {{ maskTokenKey(token.token_key) }}
                  </code>
                  <button
                    type="button"
                    class="btn btn-ghost btn-icon"
                    data-testid="token-copy-key"
                    :aria-label="
                      copiedKey === token.token_key ? t('common.copied') : t('common.copy')
                    "
                    :title="copiedKey === token.token_key ? t('common.copied') : t('common.copy')"
                    @click="copyKey(token.token_key)"
                  >
                    <UiIcon
                      :name="copiedKey === token.token_key ? 'check' : 'copy'"
                      :size="14"
                      :class="copiedKey === token.token_key ? 'text-success' : undefined"
                    />
                  </button>
                </span>
              </TableCell>
              <TableCell>
                <div class="w-36" :title="quotaLabel(token)">
                  <div class="mb-1 flex items-center justify-between font-mono text-xs">
                    <span class="text-fg-muted" data-testid="token-settled">
                      {{ formatUsdFixed2(token.settled_usd_micros) }}
                    </span>
                    <span class="font-semibold" data-testid="token-balance">
                      {{
                        token.balance_usd_micros === null
                          ? t('common.unlimited')
                          : formatUsdFixed2(token.balance_usd_micros)
                      }}
                    </span>
                  </div>
                  <div
                    v-if="quotaRatio(token) !== null"
                    class="bg-surface-alt h-1.5 w-full overflow-hidden rounded-full"
                    data-testid="token-quota-track"
                  >
                    <div
                      class="h-full rounded-full transition-[width]"
                      :class="quotaColorClass(quotaRatio(token) ?? 0)"
                      :style="{ width: `${(quotaRatio(token) ?? 0) * 100}%` }"
                    />
                  </div>
                </div>
              </TableCell>
              <TableCell
                align="center"
                class="text-fg-muted font-mono text-xs"
                data-testid="token-rpm"
              >
                {{ token.rate_limit_rpm !== null ? token.rate_limit_rpm : t('common.unlimited') }}
              </TableCell>
              <TableCell align="center">
                <button
                  type="button"
                  class="badge cursor-pointer"
                  :class="token.enabled ? 'badge-success' : 'badge-danger'"
                  data-testid="token-toggle-enabled"
                  :disabled="togglingKey === token.token_key"
                  :aria-label="token.enabled ? t('tokens.disable') : t('tokens.enable')"
                  :title="token.enabled ? t('tokens.disable') : t('tokens.enable')"
                  @click="toggleMutation.mutate(token)"
                >
                  {{ token.enabled ? t('tokens.statusEnabled') : t('tokens.statusDisabled') }}
                </button>
              </TableCell>
              <TableCell align="center" class="text-fg-muted text-xs">
                {{ formatCreated(token.created_at) }}
              </TableCell>
              <TableCell align="center" class="text-fg-muted text-xs" data-testid="token-last-used">
                {{
                  token.last_used_at === null
                    ? t('tokens.neverUsed')
                    : formatRelative(token.last_used_at)
                }}
              </TableCell>
              <TableCell align="center">
                <span class="inline-flex items-center gap-1">
                  <button
                    type="button"
                    class="btn btn-ghost btn-icon"
                    data-testid="token-edit"
                    :aria-label="t('common.edit')"
                    :title="t('common.edit')"
                    @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                    @click="openEdit(token)"
                  >
                    <UiIcon name="pencil" :size="16" />
                  </button>
                  <DataTableRowActions>
                    <DataTableMenuItem
                      danger
                      data-testid="token-delete"
                      @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                      @select="openDelete(token)"
                    >
                      {{ t('common.delete') }}
                    </DataTableMenuItem>
                  </DataTableRowActions>
                </span>
              </TableCell>
            </TableRow>
            <TableRow v-if="filteredTokens.length === 0">
              <TableCell :colspan="10" class="h-24 whitespace-normal">
                <EmptyState :title="t('common.emptyList')">
                  <button type="button" class="btn btn-primary" @click="openCreate">
                    {{ t('tokens.create') }}
                  </button>
                </EmptyState>
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
      <DataTableBulkBar
        :count="selection.count.value"
        data-testid="tokens-bulk-bar"
        @clear="selection.clear"
      >
        <button
          type="button"
          class="btn btn-danger-filled bulk-bar__delete"
          data-testid="tokens-bulk-delete"
          @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
          @click="openBulkDelete"
        >
          {{ t('common.delete') }}
        </button>
      </DataTableBulkBar>
    </div>

    <template v-for="(win, index) in windows" :key="win.id">
      <TokenEditorWindow
        v-if="win.payload.kind === 'editor'"
        :initial="win.payload.token"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
      />
      <ConfirmWindow
        v-else-if="win.payload.kind === 'delete'"
        :title="t('tokens.deleteTitle')"
        :message="t('tokens.deleteMessage', { name: win.payload.token.name })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="deleteErrors[win.id] ?? ''"
        :busy="deletingId === win.payload.token.id"
        confirm-test-id="token-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="deleteMutation.mutate(win.payload.token.id)"
      />
      <ConfirmWindow
        v-else
        :title="t('tokens.bulkDeleteTitle')"
        :message="t('tokens.bulkDeleteMessage', { count: selection.count.value })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="bulkDelete.error.value"
        :busy="bulkDelete.isPending.value"
        confirm-test-id="token-bulk-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="bulkDelete.mutate([...selection.selected.value])"
      />
    </template>
  </div>
</template>
