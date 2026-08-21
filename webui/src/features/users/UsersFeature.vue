<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { UserAdminView } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FacetedFilter from '@/components/ui/FacetedFilter.vue';
import OverflowChips from '@/components/ui/OverflowChips.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import InlineError from '@/components/ui/InlineError.vue';
import DataTable from '@/components/ui/data-table/DataTable.vue';
import DataTableMenuItem from '@/components/ui/data-table/DataTableMenuItem.vue';
import DataTableRowActions from '@/components/ui/data-table/DataTableRowActions.vue';
import DataTableToolbar from '@/components/ui/data-table/DataTableToolbar.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import { useWindowStack } from '@/composables/useWindowStack';
import { useToast } from '@/composables/useToast';
import UserEditorWindow from '@/features/users/UserEditorWindow.vue';
import UserManageWindow from '@/features/users/UserManageWindow.vue';
import { formatUsdMicros } from '@/lib/format';
import { groupDisplayName } from '@/lib/visible-models';
import { anchorFromEvent, type FloatingWindowAnchor } from '@/lib/window-anchor';

type UserManageTab = 'recharge' | 'groups' | 'tokens';

/** 创建是空表单；充值/分组/令牌都作用在已有用户上，收进同一个 Tabs 浮窗以免叠三扇窗。 */
type UserWindowPayload =
  { kind: 'create' } | { kind: 'manage'; user: UserAdminView; tab: UserManageTab };

const { t } = useI18n();
const { error } = useToast();
const queryClient = useQueryClient();
const searchText = ref('');
const roleFilter = ref<string[]>([]);
const statusFilter = ref<string[]>([]);
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
} = useWindowStack<UserWindowPayload>();

const usersQuery = useQuery({
  queryKey: ['users'],
  queryFn: () => apiClient.listUsers(),
});

const users = computed(() => usersQuery.data.value ?? []);
const showTableSkeleton = computed(() => usersQuery.isPending.value && !usersQuery.data.value);

const roleOptions = computed(() => {
  const counts = { root: 0, admin: 0, user: 0 };
  for (const user of users.value) {
    counts[user.role] += 1;
  }
  return [
    { value: 'root', label: t('users.roleRoot'), count: counts.root },
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

const filtered = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  const roles = new Set(roleFilter.value);
  const statuses = new Set(statusFilter.value);
  return users.value.filter((user) => {
    if (roles.size > 0 && !roles.has(user.role)) return false;
    if (statuses.size > 0) {
      const flag = user.enabled ? 'enabled' : 'disabled';
      if (!statuses.has(flag)) return false;
    }
    if (!q) return true;
    return user.email.toLowerCase().includes(q) || user.display_name.toLowerCase().includes(q);
  });
});

function roleLabel(role: UserAdminView['role']): string {
  if (role === 'root') return t('users.roleRoot');
  if (role === 'admin') return t('users.roleAdmin');
  return t('users.roleUser');
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

function openCreate(event: Event) {
  openWindow(anchorFromEvent(event), { kind: 'create' });
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
    if (payload.kind === 'create') continue;
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
            <template #actions>
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
        <colgroup>
          <col class="w-[22%]" />
          <col class="w-[16%]" />
          <col class="w-24" />
          <col class="w-24" />
          <col class="w-28" />
          <col />
          <col class="w-24" />
        </colgroup>
        <TableHeader>
          <TableRow>
            <TableHead>{{ t('users.email') }}</TableHead>
            <TableHead>{{ t('users.displayName') }}</TableHead>
            <TableHead>{{ t('users.role') }}</TableHead>
            <TableHead align="center">{{ t('users.status') }}</TableHead>
            <TableHead>{{ t('users.balance') }}</TableHead>
            <TableHead>{{ t('users.groups') }}</TableHead>
            <TableHead align="center">{{ t('common.actions') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" :columns="7" />
          <template v-else>
            <TableRow
              v-for="user in filtered"
              :key="user.id"
              data-testid="user-row"
              :data-user-id="String(user.id)"
            >
              <TableCell class="font-mono" truncate :title="user.email">{{ user.email }}</TableCell>
              <TableCell truncate :title="user.display_name">{{ user.display_name }}</TableCell>
              <TableCell>{{ roleLabel(user.role) }}</TableCell>
              <TableCell align="center">
                <button
                  type="button"
                  class="badge cursor-pointer"
                  :class="user.enabled ? 'badge-success' : 'badge-danger'"
                  data-testid="user-toggle-enabled"
                  :aria-label="user.enabled ? t('users.statusEnabled') : t('users.statusDisabled')"
                  :title="user.enabled ? t('users.statusEnabled') : t('users.statusDisabled')"
                  @click="toggleMutation.mutate(user)"
                >
                  {{ user.enabled ? t('users.statusEnabled') : t('users.statusDisabled') }}
                </button>
              </TableCell>
              <TableCell class="font-mono">{{
                formatUsdMicros(user.balance_usd_micros)
              }}</TableCell>
              <TableCell>
                <OverflowChips
                  :items="
                    user.assigned_groups.map((name) =>
                      groupDisplayName(name, t('models.ungrouped')),
                    )
                  "
                  chip-test-id="user-group-chip"
                />
              </TableCell>
              <TableCell align="center">
                <DataTableRowActions>
                  <DataTableMenuItem
                    data-testid="user-recharge"
                    @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                    @select="openManage('recharge', user)"
                  >
                    {{ t('users.recharge') }}
                  </DataTableMenuItem>
                  <DataTableMenuItem
                    data-testid="user-groups"
                    @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                    @select="openManage('groups', user)"
                  >
                    {{ t('users.assignGroups') }}
                  </DataTableMenuItem>
                  <DataTableMenuItem
                    data-testid="user-tokens"
                    @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                    @select="openManage('tokens', user)"
                  >
                    {{ t('users.viewTokens') }}
                  </DataTableMenuItem>
                </DataTableRowActions>
              </TableCell>
            </TableRow>
            <TableRow v-if="filtered.length === 0">
              <TableCell :colspan="7">
                <EmptyState :title="t('common.emptyList')" />
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
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
        v-else
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
    </template>
  </div>
</template>
