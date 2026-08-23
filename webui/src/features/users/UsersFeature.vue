<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { SortDir, UserAdminView } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import Checkbox from '@/components/ui/Checkbox.vue';
import ConfirmWindow from '@/components/ui/ConfirmWindow.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FacetedFilter from '@/components/ui/FacetedFilter.vue';
import InlineError from '@/components/ui/InlineError.vue';
import OverflowChips from '@/components/ui/OverflowChips.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import SelectCell from '@/components/ui/data-table/SelectCell.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import DataTable from '@/components/ui/data-table/DataTable.vue';
import DataTableBulkBar from '@/components/ui/data-table/DataTableBulkBar.vue';
import DataTableColumnHeader from '@/components/ui/data-table/DataTableColumnHeader.vue';
import DataTableMenuItem from '@/components/ui/data-table/DataTableMenuItem.vue';
import DataTableRowActions from '@/components/ui/data-table/DataTableRowActions.vue';
import DataTableToolbar from '@/components/ui/data-table/DataTableToolbar.vue';
import DataTableViewOptions from '@/components/ui/data-table/DataTableViewOptions.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import { useBulkDelete } from '@/composables/useBulkDelete';
import { useColumnVisibility, type ColumnVisibilitySpec } from '@/composables/useColumnVisibility';
import { useRowSelection } from '@/composables/useRowSelection';
import { useWindowStack } from '@/composables/useWindowStack';
import { useToast } from '@/composables/useToast';
import UserEditorWindow from '@/features/users/UserEditorWindow.vue';
import UserManageWindow from '@/features/users/UserManageWindow.vue';
import { formatCount, formatTokensCount, formatUnixMillis, formatUsdMicros } from '@/lib/format';
import { useCurrentUser } from '@/lib/session';
import { groupDisplayName } from '@/lib/visible-models';
import { anchorFromEvent, type FloatingWindowAnchor } from '@/lib/window-anchor';

type UserManageTab = 'profile' | 'recharge' | 'plan' | 'tokens';

type UserWindowPayload =
  | { kind: 'create' }
  | { kind: 'manage'; user: UserAdminView; tab: UserManageTab }
  | { kind: 'delete'; user: UserAdminView }
  | { kind: 'bulk-delete' };

type UserColumnId =
  | 'email'
  | 'displayName'
  | 'role'
  | 'balance'
  | 'rateLimitRpm'
  | 'requestCount'
  | 'tokensUsage'
  | 'lastUsedAt'
  | 'groups'
  | 'status'
  | 'actions';

type UserSortBy =
  | 'balance'
  | 'rateLimitRpm'
  | 'requestCount'
  | 'tokensUsage'
  | 'lastUsedAt';

const USER_COLUMNS: ColumnVisibilitySpec<UserColumnId>[] = [
  { id: 'email', locked: true },
  { id: 'displayName' },
  { id: 'role' },
  { id: 'balance' },
  { id: 'rateLimitRpm' },
  { id: 'requestCount' },
  { id: 'tokensUsage' },
  { id: 'lastUsedAt' },
  { id: 'groups' },
  { id: 'status' },
  { id: 'actions', locked: true },
];

const USER_HIDEABLE: UserColumnId[] = [
  'displayName',
  'role',
  'balance',
  'rateLimitRpm',
  'requestCount',
  'tokensUsage',
  'lastUsedAt',
  'groups',
  'status',
];

const { t, locale } = useI18n();
const { error } = useToast();
const queryClient = useQueryClient();
const me = useCurrentUser();
const searchText = ref('');
const roleFilter = ref<string[]>([]);
const statusFilter = ref<string[]>([]);
const groupFilter = ref<string[]>([]);
const pendingAnchor = ref<FloatingWindowAnchor | null>(null);

const sortBy = ref<UserSortBy | null>(null);
const sortDir = ref<SortDir>('asc');

const { visible, columnCount, setVisible, menuItems } = useColumnVisibility(
  'kairos-users-columns',
  USER_COLUMNS,
);

const columnMenuItems = computed(() => menuItems(USER_HIDEABLE));

const columnLabels = computed((): Record<UserColumnId, string> => ({
  email: t('users.email'),
  displayName: t('users.displayName'),
  role: t('users.role'),
  balance: t('users.balance'),
  rateLimitRpm: t('users.rateLimitRpm'),
  requestCount: t('users.requestCount'),
  tokensUsage: t('users.tokensUsage'),
  lastUsedAt: t('users.lastUsedAt'),
  groups: t('users.groups'),
  status: t('users.status'),
  actions: t('common.actions'),
}));

function sortedState(column: UserSortBy): SortDir | false {
  return sortBy.value === column ? sortDir.value : false;
}

function sortClearable(column: UserSortBy): boolean {
  return sortBy.value === column;
}

function ariaSort(column: UserSortBy): 'ascending' | 'descending' | 'none' {
  if (sortBy.value !== column) return 'none';
  return sortDir.value === 'asc' ? 'ascending' : 'descending';
}

function onSort(column: UserSortBy, dir: SortDir) {
  sortBy.value = column;
  sortDir.value = dir;
}

function onClearSort() {
  sortBy.value = null;
  sortDir.value = 'asc';
}

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
} = useWindowStack<UserWindowPayload>();

const usersQuery = useQuery({
  queryKey: ['users'],
  queryFn: () => apiClient.listUsers(),
});

const users = computed(() => usersQuery.data.value ?? []);
const showTableSkeleton = computed(() => usersQuery.isPending.value && !usersQuery.data.value);

const roleOptions = computed(() => {
  if (me.value?.role !== 'root') return [];
  const counts = { admin: 0, user: 0 };
  for (const user of users.value) {
    if (user.role === 'admin') counts.admin += 1;
    else if (user.role === 'user') counts.user += 1;
  }
  return [
    { value: 'admin', label: t('users.roleAdmin'), count: counts.admin },
    { value: 'user', label: t('users.roleUser'), count: counts.user },
  ];
});

const statusOptions = computed(() => {
  const enabled = users.value.filter((user) => user.enabled).length;
  return [
    { value: 'enabled', label: t('users.statusEnabled'), count: enabled },
    { value: 'disabled', label: t('users.statusDisabled'), count: users.value.length - enabled },
  ];
});

const groupOptions = computed(() => {
  const counts = new Map<string, number>();
  for (const user of users.value) {
    if (user.role === 'root') continue;
    for (const g of user.assigned_groups) {
      counts.set(g, (counts.get(g) ?? 0) + 1);
    }
  }
  return Array.from(counts.entries())
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([name, count]) => ({
      value: name,
      label: groupDisplayName(name, t('models.ungrouped')),
      count,
    }));
});

const filtered = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  const roles = new Set(roleFilter.value);
  const statuses = new Set(statusFilter.value);
  const groups = new Set(groupFilter.value);
  return users.value.filter((user) => {
    if (roles.size > 0 && !roles.has(user.role)) return false;
    if (statuses.size > 0) {
      const flag = user.enabled ? 'enabled' : 'disabled';
      if (!statuses.has(flag)) return false;
    }
    if (groups.size > 0) {
      if (user.role === 'root') return false;
      const matchesGroup = user.assigned_groups.some((g) => groups.has(g));
      if (!matchesGroup) return false;
    }
    if (!q) return true;
    return user.email.toLowerCase().includes(q) || user.display_name.toLowerCase().includes(q);
  });
});

const sortedAndFiltered = computed(() => {
  const list = [...filtered.value];
  const col = sortBy.value;
  if (!col) return list;
  const dir = sortDir.value === 'asc' ? 1 : -1;

  return list.sort((a, b) => {
    switch (col) {
      case 'balance':
        return (a.balance_usd_micros - b.balance_usd_micros) * dir;
      case 'rateLimitRpm': {
        const rpmA = a.rate_limit_rpm ?? 0;
        const rpmB = b.rate_limit_rpm ?? 0;
        return (rpmA - rpmB) * dir;
      }
      case 'requestCount':
        return (a.request_count - b.request_count) * dir;
      case 'tokensUsage': {
        const tokA = a.input_tokens + a.output_tokens;
        const tokB = b.input_tokens + b.output_tokens;
        return (tokA - tokB) * dir;
      }
      case 'lastUsedAt': {
        const lastA = a.last_used_at ?? 0;
        const lastB = b.last_used_at ?? 0;
        return (lastA - lastB) * dir;
      }
      default:
        return 0;
    }
  });
});

const visibleColumnCount = computed(() => columnCount.value + 1);

const selection = useRowSelection<number>();

const allVisibleSelected = computed({
  get: () =>
    sortedAndFiltered.value.length > 0 &&
    sortedAndFiltered.value.every((user) => selection.isSelected(user.id)),
  set: (value) =>
    selection.setMany(
      sortedAndFiltered.value.map((user) => user.id),
      value,
    ),
});

const someVisibleSelected = computed(() =>
  sortedAndFiltered.value.some((user) => selection.isSelected(user.id)),
);

watch(users, (rows) => selection.prune(rows.map((row) => row.id)));

function roleLabel(role: UserAdminView['role']): string {
  if (role === 'root') return t('users.roleRoot');
  if (role === 'admin') return t('users.roleAdmin');
  return t('users.roleUser');
}

function roleBadgeClass(role: UserAdminView['role']): string {
  if (role === 'root') return 'badge-unified';
  if (role === 'admin') return 'badge-info';
  return 'badge-neutral';
}

const toggleMutation = useMutation({
  mutationFn: (user: UserAdminView) => apiClient.updateUser(user.id, { enabled: !user.enabled }),
  onSuccess: async () => {
    await queryClient.invalidateQueries({ queryKey: ['users'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

const deleteErrors = ref<Record<number, string>>({});
const deletingUserId = ref<number | null>(null);

const deleteMutation = useMutation({
  mutationFn: async (user: UserAdminView) => {
    deletingUserId.value = user.id;
    await apiClient.deleteUser(user.id);
  },
  onSuccess: async (_data, user) => {
    selection.setMany([user.id], false);
    const entry = windows.value.find(
      (win) => win.payload.kind === 'delete' && win.payload.user.id === user.id,
    );
    if (entry) closeWindow(entry.id);
    await queryClient.invalidateQueries({ queryKey: ['users'] });
  },
  onError: (err, user) => {
    const entry = windows.value.find(
      (win) => win.payload.kind === 'delete' && win.payload.user.id === user.id,
    );
    if (entry) deleteErrors.value[entry.id] = extractApiError(err).message;
    else error(extractApiError(err).message);
  },
  onSettled: () => {
    deletingUserId.value = null;
  },
});

const bulkDelete = useBulkDelete<number>({
  selection,
  windowStack: { windows, close: closeWindow },
  queryKey: ['users'],
  deleteOne: (userId) => apiClient.deleteUser(userId),
});

function openCreate(event: Event) {
  openWindow(anchorFromEvent(event), { kind: 'create' });
}

function openEdit(user: UserAdminView) {
  openManage('profile', user);
}

function openDelete(user: UserAdminView) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'delete' && entry.payload.user.id === user.id,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  const entry = openWindow(takePendingAnchor(), { kind: 'delete', user });
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

function openManage(tab: UserManageTab, user: UserAdminView) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'manage' && entry.payload.user.id === user.id,
  );
  if (existing && existing.payload.kind === 'manage') {
    existing.payload.tab = tab;
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'manage', user, tab });
}

watch(users, (rows) => {
  for (const entry of windows.value) {
    const payload = entry.payload;
    if (payload.kind === 'bulk-delete' || payload.kind === 'create') continue;
    const latest = rows.find((user) => user.id === payload.user.id);
    if (!latest) closeWindow(entry.id);
    else payload.user = latest;
  }
});
</script>

<template>
  <div class="flex flex-col">
    <PageHeader :title="t('nav.users')" />

    <InlineError
      v-if="usersQuery.isError.value && !usersQuery.data.value"
      :message="extractApiError(usersQuery.error.value).message"
      @retry="() => usersQuery.refetch()"
    />

    <div v-else class="flex flex-col">
      <DataTable class="[&_[data-slot=table]]:table-fixed" :busy="showTableSkeleton">
        <template #toolbar>
          <DataTableToolbar>
            <SearchInput
              id="users-search"
              v-model="searchText"
              class="max-w-sm"
              data-testid="users-search"
              :placeholder="t('users.search')"
              :aria-label="t('users.search')"
            />
            <FacetedFilter
              v-if="roleOptions.length > 0"
              v-model="roleFilter"
              :title="t('users.role')"
              :options="roleOptions"
              test-id="users-role-filter"
            />
            <FacetedFilter
              v-model="statusFilter"
              :title="t('users.status')"
              :options="statusOptions"
              test-id="users-status-filter"
            />
            <FacetedFilter
              v-if="groupOptions.length > 0"
              v-model="groupFilter"
              :title="t('users.groups')"
              :options="groupOptions"
              test-id="users-group-filter"
            />
            <template #actions>
              <DataTableViewOptions
                :items="columnMenuItems"
                :labels="columnLabels"
                test-id="users-columns"
                @toggle="setVisible"
              />
              <button
                type="button"
                class="btn btn-primary"
                data-testid="create-user"
                @click="openCreate"
              >
                {{ t('users.create') }}
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
                  data-testid="users-select-all"
                  :aria-label="t('common.selectAll')"
                />
              </div>
            </TableHead>
            <TableHead v-if="visible.email">{{ t('users.email') }}</TableHead>
            <TableHead v-if="visible.displayName">{{ t('users.displayName') }}</TableHead>
            <TableHead v-if="visible.role" class="w-24">{{ t('users.role') }}</TableHead>
            <TableHead v-if="visible.balance" class="w-28" :aria-sort="ariaSort('balance')">
              <DataTableColumnHeader
                :label="t('users.balance')"
                :sorted="sortedState('balance')"
                :clearable="sortClearable('balance')"
                @sort="onSort('balance', $event)"
                @clear="onClearSort"
              />
            </TableHead>
            <TableHead v-if="visible.rateLimitRpm" class="w-36" :aria-sort="ariaSort('rateLimitRpm')">
              <DataTableColumnHeader
                :label="t('users.rateLimitRpm')"
                :sorted="sortedState('rateLimitRpm')"
                :clearable="sortClearable('rateLimitRpm')"
                @sort="onSort('rateLimitRpm', $event)"
                @clear="onClearSort"
              />
            </TableHead>
            <TableHead v-if="visible.requestCount" class="w-28" :aria-sort="ariaSort('requestCount')">
              <DataTableColumnHeader
                :label="t('users.requestCount')"
                :sorted="sortedState('requestCount')"
                :clearable="sortClearable('requestCount')"
                @sort="onSort('requestCount', $event)"
                @clear="onClearSort"
              />
            </TableHead>
            <TableHead v-if="visible.tokensUsage" class="w-28" :aria-sort="ariaSort('tokensUsage')">
              <DataTableColumnHeader
                :label="t('users.tokensUsage')"
                :sorted="sortedState('tokensUsage')"
                :clearable="sortClearable('tokensUsage')"
                @sort="onSort('tokensUsage', $event)"
                @clear="onClearSort"
              />
            </TableHead>
            <TableHead v-if="visible.lastUsedAt" class="w-40" :aria-sort="ariaSort('lastUsedAt')">
              <DataTableColumnHeader
                :label="t('users.lastUsedAt')"
                :sorted="sortedState('lastUsedAt')"
                :clearable="sortClearable('lastUsedAt')"
                @sort="onSort('lastUsedAt', $event)"
                @clear="onClearSort"
              />
            </TableHead>
            <TableHead v-if="visible.groups">{{ t('users.groups') }}</TableHead>
            <TableHead v-if="visible.status" class="w-24" align="center">{{ t('users.status') }}</TableHead>
            <TableHead v-if="visible.actions" class="w-24" align="center">{{ t('common.actions') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" :columns="visibleColumnCount" />
          <template v-else>
            <TableRow
              v-for="user in sortedAndFiltered"
              :key="user.id"
              data-testid="user-row"
              :data-user-id="String(user.id)"
              :data-state="selection.isSelected(user.id) ? 'selected' : undefined"
            >
              <SelectCell
                :checked="selection.isSelected(user.id)"
                test-id="user-select"
                @toggle="selection.toggle(user.id)"
              />
              <TableCell v-if="visible.email" class="font-mono" truncate :title="user.email">{{ user.email }}</TableCell>
              <TableCell v-if="visible.displayName" truncate :title="user.display_name">{{ user.display_name }}</TableCell>
              <TableCell v-if="visible.role">
                <span class="badge" :class="roleBadgeClass(user.role)">
                  {{ roleLabel(user.role) }}
                </span>
              </TableCell>
              <TableCell v-if="visible.balance" class="font-mono">{{
                formatUsdMicros(user.balance_usd_micros)
              }}</TableCell>
              <TableCell v-if="visible.rateLimitRpm" class="font-mono">
                <span v-if="user.rate_limit_rpm && user.rate_limit_rpm > 0">
                  {{ formatCount(user.rate_limit_rpm, locale) }}
                </span>
                <span v-else class="badge badge-neutral text-xs font-mono">
                  {{ t('common.unlimited') }}
                </span>
              </TableCell>
              <TableCell v-if="visible.requestCount" class="font-mono">
                {{ formatCount(user.request_count || 0, locale) }}
              </TableCell>
              <TableCell
                v-if="visible.tokensUsage"
                class="font-mono"
                :title="`Input: ${formatCount(user.input_tokens || 0, locale)} / Output: ${formatCount(user.output_tokens || 0, locale)}`"
              >
                {{ formatTokensCount((user.input_tokens || 0) + (user.output_tokens || 0)) }}
              </TableCell>
              <TableCell v-if="visible.lastUsedAt" class="font-mono text-xs text-[var(--fg-muted)]">
                <span v-if="user.last_used_at">
                  {{ formatUnixMillis(user.last_used_at, locale) }}
                </span>
                <span v-else class="text-[var(--fg-muted)]">
                  {{ t('users.neverUsed') }}
                </span>
              </TableCell>
              <TableCell v-if="visible.groups">
                <span v-if="user.role === 'root'" class="badge badge-neutral font-mono text-xs">
                  {{ t('common.unlimited') }}
                </span>
                <OverflowChips
                  v-else
                  :items="
                    user.assigned_groups.map((name) =>
                      groupDisplayName(name, t('models.ungrouped')),
                    )
                  "
                  chip-test-id="user-group-chip"
                />
              </TableCell>
              <TableCell v-if="visible.status" align="center">
                <button
                  type="button"
                  class="badge"
                  :class="[
                    user.enabled ? 'badge-success' : 'badge-danger',
                    user.role === 'root' ? 'cursor-not-allowed opacity-80' : 'cursor-pointer',
                  ]"
                  data-testid="user-toggle-enabled"
                  :disabled="user.role === 'root'"
                  :aria-label="user.enabled ? t('users.statusEnabled') : t('users.statusDisabled')"
                  :title="
                    user.role === 'root'
                      ? t('users.rootEnabledProtected')
                      : user.enabled
                        ? t('users.statusEnabled')
                        : t('users.statusDisabled')
                  "
                  @click="user.role !== 'root' && toggleMutation.mutate(user)"
                >
                  {{ user.enabled ? t('users.statusEnabled') : t('users.statusDisabled') }}
                </button>
              </TableCell>
              <TableCell v-if="visible.actions" align="center">
                <span class="inline-flex items-center gap-1">
                  <button
                    type="button"
                    class="btn btn-ghost btn-icon"
                    data-testid="user-edit"
                    :aria-label="t('common.edit')"
                    :title="t('common.edit')"
                    @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                    @click="openEdit(user)"
                  >
                    <UiIcon name="pencil" :size="16" />
                  </button>
                  <DataTableRowActions v-if="user.role !== 'root'">
                    <DataTableMenuItem
                      danger
                      data-testid="user-delete"
                      @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                      @select="openDelete(user)"
                    >
                      {{ t('users.delete') }}
                    </DataTableMenuItem>
                  </DataTableRowActions>
                </span>
              </TableCell>
            </TableRow>
            <TableRow v-if="sortedAndFiltered.length === 0">
              <TableCell :colspan="visibleColumnCount">
                <EmptyState :title="t('common.emptyList')" />
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
      <DataTableBulkBar
        :count="selection.count.value"
        data-testid="users-bulk-bar"
        @clear="selection.clear"
      >
        <button
          type="button"
          class="btn btn-danger-filled bulk-bar__delete"
          data-testid="users-bulk-delete"
          @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
          @click="openBulkDelete"
        >
          {{ t('common.delete') }}
        </button>
      </DataTableBulkBar>
    </div>

    <template v-for="(win, index) in windows" :key="win.id">
      <UserEditorWindow
        v-if="win.payload.kind === 'create'"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
      />
      <UserManageWindow
        v-else-if="win.payload.kind === 'manage'"
        :user="win.payload.user"
        :tab="win.payload.tab"
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
        :title="t('users.deleteTitle')"
        :message="t('users.deleteMessage', { name: win.payload.user.display_name })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="deleteErrors[win.id] ?? ''"
        :busy="deletingUserId === win.payload.user.id"
        confirm-test-id="user-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="deleteMutation.mutate(win.payload.user)"
      />
      <ConfirmWindow
        v-else
        :title="t('users.bulkDeleteTitle')"
        :message="t('users.bulkDeleteMessage', { count: selection.count.value })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="bulkDelete.error.value"
        :busy="bulkDelete.isPending.value"
        confirm-test-id="user-bulk-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="bulkDelete.mutate([...selection.selected.value])"
      />
    </template>
  </div>
</template>
