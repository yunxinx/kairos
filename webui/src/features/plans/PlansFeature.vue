<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useSearch } from '@tanstack/vue-router';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { PlanAudience, PlanView } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import Checkbox from '@/components/ui/Checkbox.vue';
import ConfirmWindow from '@/components/ui/ConfirmWindow.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FacetedFilter from '@/components/ui/FacetedFilter.vue';
import InlineError from '@/components/ui/InlineError.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import DataTable from '@/components/ui/data-table/DataTable.vue';
import DataTableBulkBar from '@/components/ui/data-table/DataTableBulkBar.vue';
import DataTableMenuItem from '@/components/ui/data-table/DataTableMenuItem.vue';
import DataTableRowActions from '@/components/ui/data-table/DataTableRowActions.vue';
import DataTableToolbar from '@/components/ui/data-table/DataTableToolbar.vue';
import DataTableViewOptions from '@/components/ui/data-table/DataTableViewOptions.vue';
import SelectCell from '@/components/ui/data-table/SelectCell.vue';
import OverflowChips from '@/components/ui/OverflowChips.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import { useBulkDelete, type BulkDeletePayload } from '@/composables/useBulkDelete';
import { useColumnVisibility, type ColumnVisibilitySpec } from '@/composables/useColumnVisibility';
import { useRowSelection } from '@/composables/useRowSelection';
import { useWindowStack } from '@/composables/useWindowStack';
import { useToast } from '@/composables/useToast';
import PlanEditorWindow from '@/features/plans/PlanEditorWindow.vue';
import { formatDiscountBp, formatUnixMillis, formatUsdMicros } from '@/lib/format';
import { anchorFromEvent, type FloatingWindowAnchor } from '@/lib/window-anchor';

type PlanWindowPayload =
  /** 新建时 `plan` 为 null，受众由点的是哪个按钮决定；编辑时受众取自 `plan`。 */
  | { kind: 'editor'; plan: PlanView | null; audience: PlanAudience }
  | { kind: 'delete'; plan: PlanView }
  | BulkDeletePayload;

type PlanColumnId = 'note' | 'initialGrant' | 'groups' | 'createdAt';

const PLAN_COLUMNS: ColumnVisibilitySpec<PlanColumnId>[] = [
  { id: 'note' },
  { id: 'initialGrant' },
  { id: 'groups' },
  { id: 'createdAt' },
];

const { t, locale } = useI18n();
const { error } = useToast();
const queryClient = useQueryClient();
const pendingAnchor = ref<FloatingWindowAnchor | null>(null);
const routeSearch = useSearch({ from: '/plans' });
const searchText = ref(routeSearch.value.q ?? '');
const audienceFilter = ref<string[]>([]);
const flagFilter = ref<string[]>([]);
const { visible, columnCount, setVisible, menuItems } = useColumnVisibility(
  'kairos-plans-columns',
  PLAN_COLUMNS,
);
const columnMenuItems = computed(() => menuItems(PLAN_COLUMNS.map((column) => column.id)));
const columnLabels = computed((): Record<PlanColumnId, string> => ({
  note: t('plans.note'),
  initialGrant: t('plans.initialGrant'),
  groups: t('plans.modelGroups'),
  createdAt: t('plans.createdAt'),
}));
const visibleColumnCount = computed(() => 8 + columnCount.value);

watch(
  () => routeSearch.value.q,
  (nextQ) => {
    searchText.value = nextQ ?? '';
  },
);

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
} = useWindowStack<PlanWindowPayload>();

const plansQuery = useQuery({
  queryKey: ['plans'],
  queryFn: () => apiClient.listPlans(),
});

const plans = computed(() => plansQuery.data.value ?? []);
const showTableSkeleton = computed(() => plansQuery.isPending.value && !plansQuery.data.value);

const audienceOptions = computed(() => {
  const admin = plans.value.filter((plan) => plan.audience === 'admin').length;
  return [
    { value: 'user', label: t('plans.audienceUser'), count: plans.value.length - admin },
    { value: 'admin', label: t('plans.audienceAdmin'), count: admin },
  ];
});

/**
 * 属性筛选：三个开关位各自独立，同一组内取并集（与其余资源页的分面筛选一致）。
 *
 * 只提供「是」侧选项：三个位的否定面都是默认多数，把「非默认」之类也摆上去
 * 只会让选项翻倍而筛不出东西。
 */
const flagOptions = computed(() => [
  {
    value: 'default',
    label: t('plans.defaultBadge'),
    count: plans.value.filter((plan) => plan.is_default).length,
  },
  {
    value: 'shared',
    label: t('plans.sharedWithAdmin'),
    count: plans.value.filter((plan) => plan.shared_with_admin).length,
  },
  {
    value: 'builtin',
    label: t('plans.builtin'),
    count: plans.value.filter((plan) => plan.builtin).length,
  },
]);

const filteredPlans = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  const audiences = new Set(audienceFilter.value);
  const flags = new Set(flagFilter.value);
  return plans.value.filter((plan) => {
    if (audiences.size > 0 && !audiences.has(plan.audience)) return false;
    if (flags.size > 0) {
      const matches =
        (flags.has('default') && plan.is_default) ||
        (flags.has('shared') && plan.shared_with_admin) ||
        (flags.has('builtin') && plan.builtin);
      if (!matches) return false;
    }
    if (!q) return true;
    return (
      plan.display_name.toLowerCase().includes(q) ||
      plan.note.toLowerCase().includes(q) ||
      plan.groups.some((name) => name.toLowerCase().includes(q))
    );
  });
});

// 行选择：内置档不可删，所以也不可选——否则全选会凑出一批必定失败的删除。
const selection = useRowSelection<number>();
const selectablePlans = computed(() => filteredPlans.value.filter((plan) => !plan.builtin));

const allVisibleSelected = computed({
  get: () =>
    selectablePlans.value.length > 0 &&
    selectablePlans.value.every((plan) => selection.isSelected(plan.id)),
  set: (value) =>
    selection.setMany(
      selectablePlans.value.map((plan) => plan.id),
      value,
    ),
});

const someVisibleSelected = computed(() =>
  selectablePlans.value.some((plan) => selection.isSelected(plan.id)),
);

// 删除或刷新后列表键变化，剔除幽灵选择。
watch(plans, (rows) => selection.prune(rows.map((row) => row.id)));

const bulkDelete = useBulkDelete<number>({
  selection,
  windowStack: { windows, close: closeWindow },
  queryKey: ['plans'],
  // 删档会把挂在其上的用户改挂当前受众默认档，用户列表的套餐列随之陈旧。
  alsoInvalidate: [['users']],
  deleteMany: (ids) => apiClient.deletePlans(ids, true),
});

const deleteErrors = ref<Record<number, string>>({});
const deletingId = ref<number | null>(null);

const deleteMutation = useMutation({
  mutationFn: async (plan: PlanView) => {
    deletingId.value = plan.id;
    await apiClient.deletePlan(plan.id, true);
  },
  onSuccess: async (_data, plan) => {
    const entry = windows.value.find(
      (win) => win.payload.kind === 'delete' && win.payload.plan.id === plan.id,
    );
    if (entry) closeWindow(entry.id, true);
    await queryClient.invalidateQueries({ queryKey: ['plans'] });
  },
  onError: (err, plan) => {
    const entry = windows.value.find(
      (win) => win.payload.kind === 'delete' && win.payload.plan.id === plan.id,
    );
    if (entry) deleteErrors.value[entry.id] = extractApiError(err).message;
    else error(extractApiError(err).message);
  },
  onSettled: () => {
    deletingId.value = null;
  },
});

const setDefaultMutation = useMutation({
  mutationFn: (plan: PlanView) => apiClient.setPlanDefault(plan.id),
  onSuccess: async () => {
    await queryClient.invalidateQueries({ queryKey: ['plans'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

const settingDefaultId = computed(() =>
  setDefaultMutation.isPending.value ? (setDefaultMutation.variables.value?.id ?? null) : null,
);

/** 新建：受众由点的是哪个按钮决定，同受众的新建窗只留一个。 */
function openCreate(event: Event, audience: PlanAudience) {
  const existing = windows.value.find(
    (entry) =>
      entry.payload.kind === 'editor' &&
      entry.payload.plan === null &&
      entry.payload.audience === audience,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(anchorFromEvent(event), { kind: 'editor', plan: null, audience });
}

function openEdit(plan: PlanView) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'editor' && entry.payload.plan?.id === plan.id,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'editor', plan, audience: plan.audience });
}

function openDelete(plan: PlanView) {
  if (plan.builtin) return;
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'delete' && entry.payload.plan.id === plan.id,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  const entry = openWindow(takePendingAnchor(), { kind: 'delete', plan });
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

watch(plans, (rows) => {
  for (const entry of windows.value) {
    const payload = entry.payload;
    if (payload.kind === 'bulk-delete') continue;
    const planId = payload.kind === 'editor' ? payload.plan?.id : payload.plan.id;
    const latest = rows.find((plan) => plan.id === planId);
    if (!latest && payload.kind === 'delete') continue;
    if (!latest && payload.kind === 'editor') closeWindow(entry.id, true);
    else if (latest && payload.kind === 'editor') payload.plan = latest;
  }
});
</script>

<template>
  <div class="flex flex-col">
    <PageHeader :title="t('nav.plans')" />

    <InlineError
      v-if="plansQuery.isError.value && !plansQuery.data.value"
      :message="extractApiError(plansQuery.error.value).message"
      @retry="() => plansQuery.refetch()"
    />

    <div v-else class="flex flex-col">
      <DataTable :busy="showTableSkeleton">
        <template #toolbar>
          <DataTableToolbar>
            <SearchInput
              id="plans-search"
              v-model="searchText"
              class="max-w-sm"
              data-testid="plans-search"
              :placeholder="t('plans.search')"
              :aria-label="t('plans.search')"
            />
            <FacetedFilter
              v-model="audienceFilter"
              :title="t('plans.audience')"
              :options="audienceOptions"
              test-id="plans-audience-filter"
            />
            <FacetedFilter
              v-model="flagFilter"
              :title="t('plans.attributes')"
              :options="flagOptions"
              test-id="plans-flag-filter"
            />
            <template #actions>
              <DataTableViewOptions
                :items="columnMenuItems"
                :labels="columnLabels"
                test-id="plans-columns"
                @toggle="setVisible"
              />
              <!--
                两个按钮而非一个下拉：受众建后不可改（改档会让已挂载用户悄悄增减
                管理能力），所以在入口就把选择摊开，而不是藏在表单里的一个字段。
              -->
              <button
                type="button"
                class="btn"
                data-testid="create-plan-user"
                @click="openCreate($event, 'user')"
              >
                {{ t('plans.createUser') }}
              </button>
              <button
                type="button"
                class="btn btn-primary"
                data-testid="create-plan-admin"
                @click="openCreate($event, 'admin')"
              >
                {{ t('plans.createAdmin') }}
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
                  :disabled="selectablePlans.length === 0"
                  data-testid="plans-select-all"
                  :aria-label="t('common.selectAll')"
                />
              </div>
            </TableHead>
            <TableHead>{{ t('plans.displayName') }}</TableHead>
            <TableHead>{{ t('plans.audience') }}</TableHead>
            <TableHead v-if="visible.note">{{ t('plans.note') }}</TableHead>
            <TableHead>{{ t('plans.discount') }}</TableHead>
            <TableHead>{{ t('plans.defaultRpm') }}</TableHead>
            <TableHead>{{ t('plans.sharedRpm') }}</TableHead>
            <TableHead v-if="visible.initialGrant">{{ t('plans.initialGrant') }}</TableHead>
            <TableHead v-if="visible.groups">{{ t('plans.modelGroups') }}</TableHead>
            <TableHead>{{ t('plans.share') }}</TableHead>
            <TableHead v-if="visible.createdAt">{{ t('plans.createdAt') }}</TableHead>
            <TableHead align="center">{{ t('common.actions') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton
            v-if="showTableSkeleton"
            has-select-column
            :columns="visibleColumnCount"
          />
          <template v-else>
            <TableRow
              v-for="plan in filteredPlans"
              :key="plan.id"
              data-testid="plan-row"
              :data-plan-id="String(plan.id)"
              :data-state="selection.isSelected(plan.id) ? 'selected' : undefined"
            >
              <SelectCell
                :checked="selection.isSelected(plan.id)"
                :disabled="plan.builtin"
                test-id="plan-select"
                @toggle="selection.toggle(plan.id)"
              />
              <TableCell data-testid="plan-display">
                <span class="inline-flex min-w-0 items-center gap-1.5">
                  <span class="min-w-0 shrink truncate">{{ plan.display_name }}</span>
                  <span v-if="plan.builtin" class="badge badge-neutral shrink-0 text-[10px]">
                    {{ t('plans.builtin') }}
                  </span>
                  <!-- 默认档：新用户会落到这一档，运营最常问的就是「现在默认是哪个」。 -->
                  <span
                    v-if="plan.is_default"
                    class="badge badge-success shrink-0 text-[10px] whitespace-nowrap"
                    data-testid="plan-default-badge"
                  >
                    {{ t('plans.defaultBadge') }}
                  </span>
                </span>
              </TableCell>
              <TableCell data-testid="plan-audience">
                <span
                  class="badge"
                  :class="plan.audience === 'admin' ? 'badge-info' : 'badge-neutral'"
                >
                  {{
                    plan.audience === 'admin' ? t('plans.audienceAdmin') : t('plans.audienceUser')
                  }}
                </span>
              </TableCell>
              <TableCell
                v-if="visible.note"
                class="max-w-56"
                truncate
                :title="plan.note"
                data-testid="plan-note-cell"
              >
                {{ plan.note || '-' }}
              </TableCell>
              <TableCell class="font-mono" data-testid="plan-discount-cell">
                {{ formatDiscountBp(plan.discount_bp) }}
              </TableCell>
              <TableCell class="font-mono">
                <span v-if="plan.default_rpm">{{ plan.default_rpm }}</span>
                <span v-else class="text-fg-muted">-</span>
              </TableCell>
              <TableCell class="font-mono">
                <span v-if="plan.shared_rpm">{{ plan.shared_rpm }}</span>
                <span v-else class="text-fg-muted">-</span>
              </TableCell>
              <TableCell
                v-if="visible.initialGrant"
                class="font-mono"
                data-testid="plan-initial-grant"
              >
                {{ formatUsdMicros(plan.initial_grant_usd_micros) }}
              </TableCell>
              <TableCell v-if="visible.groups" data-testid="plan-groups">
                <OverflowChips :items="plan.groups" />
              </TableCell>
              <TableCell>
                <span
                  v-if="plan.shared_with_admin"
                  class="badge badge-success"
                  data-testid="plan-shared-badge"
                >
                  {{ t('common.yes') }}
                </span>
                <span v-else class="badge badge-neutral" data-testid="plan-shared-badge">
                  {{ t('common.no') }}
                </span>
              </TableCell>
              <TableCell
                v-if="visible.createdAt"
                class="font-mono text-xs"
                data-testid="plan-created-at"
              >
                {{ formatUnixMillis(plan.created_at, locale) }}
              </TableCell>
              <TableCell align="center">
                <span class="inline-flex items-center gap-1">
                  <button
                    type="button"
                    class="btn btn-ghost btn-icon"
                    data-testid="plan-edit"
                    :aria-label="t('common.edit')"
                    :title="t('common.edit')"
                    @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                    @click="openEdit(plan)"
                  >
                    <UiIcon name="pencil" :size="16" />
                  </button>
                  <DataTableRowActions v-if="!plan.is_default || !plan.builtin">
                    <DataTableMenuItem
                      v-if="!plan.is_default"
                      :disabled="settingDefaultId === plan.id"
                      data-testid="plan-set-default"
                      @select="setDefaultMutation.mutate(plan)"
                    >
                      {{ t('plans.setDefault') }}
                    </DataTableMenuItem>
                    <DataTableMenuItem
                      v-if="!plan.builtin"
                      danger
                      data-testid="plan-delete"
                      @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                      @select="openDelete(plan)"
                    >
                      {{ t('common.delete') }}
                    </DataTableMenuItem>
                  </DataTableRowActions>
                </span>
              </TableCell>
            </TableRow>
            <TableRow v-if="filteredPlans.length === 0">
              <TableCell :colspan="visibleColumnCount" class="h-24 whitespace-normal">
                <!-- 一条都没有 vs 筛没了是两回事：后者给「新建」会把用户引向错误动作。 -->
                <EmptyState :title="plans.length === 0 ? t('plans.empty') : t('common.emptyList')">
                  <button
                    v-if="plans.length === 0"
                    type="button"
                    class="btn btn-primary"
                    data-testid="create-user-plan-empty"
                    @click="openCreate($event, 'user')"
                  >
                    {{ t('plans.createUser') }}
                  </button>
                </EmptyState>
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>

      <DataTableBulkBar
        :count="selection.count.value"
        data-testid="plans-bulk-bar"
        @clear="selection.clear"
      >
        <button
          type="button"
          class="btn btn-danger-filled bulk-bar__delete"
          data-testid="plans-bulk-delete"
          @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
          @click="openBulkDelete"
        >
          {{ t('common.delete') }}
        </button>
      </DataTableBulkBar>
    </div>

    <template v-for="(win, index) in windows" :key="win.id">
      <PlanEditorWindow
        v-if="win.payload.kind === 'editor'"
        :initial="win.payload.plan"
        :audience="win.payload.audience"
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
        :title="t('plans.deleteTitle')"
        :message="t('plans.deleteMessage', { name: win.payload.plan.display_name })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="deleteErrors[win.id] ?? ''"
        :busy="deletingId === win.payload.plan.id"
        confirm-test-id="plan-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty, false)"
        @confirm="deleteMutation.mutate(win.payload.plan)"
      />
      <ConfirmWindow
        v-else
        :title="t('plans.bulkDeleteTitle')"
        :message="t('plans.bulkDeleteMessage', { count: selection.count.value })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="bulkDelete.error.value"
        :busy="bulkDelete.isPending.value"
        confirm-test-id="plan-bulk-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty, false)"
        @confirm="bulkDelete.mutate([...selection.selected.value])"
      />
    </template>
  </div>
</template>
