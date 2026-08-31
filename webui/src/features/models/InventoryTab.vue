<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import Checkbox from '@/components/ui/Checkbox.vue';
import ConfirmWindow from '@/components/ui/ConfirmWindow.vue';
import CopyableName from '@/components/ui/CopyableName.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FacetedFilter from '@/components/ui/FacetedFilter.vue';
import InlineError from '@/components/ui/InlineError.vue';
import OverflowChips from '@/components/ui/OverflowChips.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import DataTable from '@/components/ui/data-table/DataTable.vue';
import DataTableBulkBar from '@/components/ui/data-table/DataTableBulkBar.vue';
import DataTableMenuItem from '@/components/ui/data-table/DataTableMenuItem.vue';
import DataTableMenuSeparator from '@/components/ui/data-table/DataTableMenuSeparator.vue';
import DataTableRowActions from '@/components/ui/data-table/DataTableRowActions.vue';
import DataTableToolbar from '@/components/ui/data-table/DataTableToolbar.vue';
import SelectCell from '@/components/ui/data-table/SelectCell.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import { type BulkDeletePayload } from '@/composables/useBulkDelete';
import { invalidateChannelCaches, useChannelDirectory } from '@/composables/useChannelDirectory';
import { useRowSelection } from '@/composables/useRowSelection';
import { useWindowStack } from '@/composables/useWindowStack';
import { useToast } from '@/composables/useToast';
import CatalogFillWindow from '@/features/models/CatalogFillWindow.vue';
import PriceEditorWindow from '@/features/models/PriceEditorWindow.vue';
import { hasCapability } from '@/lib/capabilities';
import { formatUsdAmount } from '@/lib/format';
import {
  aliasChips,
  buildInventory,
  inventoryRowKey,
  sectionInventory,
  sortInventory,
  type AliasChip,
  type InventoryRow,
  type InventorySection,
} from '@/lib/inventory';
import { useCurrentUser } from '@/lib/session';
import { anchorFromEvent, type FloatingWindowAnchor } from '@/lib/window-anchor';

type InventoryDeleteTarget = { name: string; channelId: number; channelName: string };
type InventorySectionRow = InventoryRow & { aliasChipItems: AliasChip[] };

type InventoryWindowPayload =
  | { kind: 'editor'; row: InventoryRow }
  | { kind: 'delete'; row: InventoryRow; channelName: string }
  | { kind: 'delete-price'; row: InventoryRow }
  | { kind: 'catalog' }
  | BulkDeletePayload;

const { t } = useI18n();
const { error } = useToast();
const queryClient = useQueryClient();
const me = useCurrentUser();
const canEditPrices = computed(() => hasCapability(me.value, 'edit_prices'));
const canEditCatalog = computed(() => hasCapability(me.value, 'edit_price_catalog'));

const searchText = ref('');
const statusFilter = ref<string[]>([]);
const selectedChannels = ref<string[]>([]);
const pendingAnchor = ref<FloatingWindowAnchor | null>(null);
const canRewriteChannels = computed(() => me.value?.role === 'root');
const canSelectRows = computed(() => canEditCatalog.value || canRewriteChannels.value);
const hasActions = computed(() => canEditPrices.value || canRewriteChannels.value);
const tableColumnCount = computed(
  () => 6 + (canSelectRows.value ? 1 : 0) + (hasActions.value ? 1 : 0),
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
} = useWindowStack<InventoryWindowPayload>();

const deleteErrors = ref<Record<number, string>>({});

const { query: channelsQuery, channels } = useChannelDirectory();
const pricesQuery = useQuery({
  queryKey: ['prices'],
  queryFn: () => apiClient.listPrices(),
});

/**
 * 清单行删除要整体改写渠道定义（`PUT /channels/{id}`），那是 root-only 的写路径。
 * 因此只有 root 才拉完整定义，也只有 root 才渲染删除入口；带 `edit_prices` 的
 * admin 仍能改价，但不给它一个注定 403 的按钮。
 */
const loadError = computed(() => channelsQuery.isError.value || pricesQuery.isError.value);
const showTableSkeleton = computed(
  () =>
    (channelsQuery.isPending.value && !channelsQuery.data.value) ||
    (pricesQuery.isPending.value && !pricesQuery.data.value),
);

const inventory = computed(() => buildInventory(channels.value, pricesQuery.data.value ?? []));

const filteredRows = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  const statuses = new Set(statusFilter.value);
  const channels = new Set(selectedChannels.value);
  const rows = inventory.value.filter((row) => {
    if (channels.size > 0 && !channels.has(row.channelName)) {
      return false;
    }
    if (statuses.size > 0) {
      const priced = row.price !== null;
      if (priced && !statuses.has('priced')) return false;
      if (!priced && !statuses.has('unpriced')) return false;
    }
    if (!q) return true;
    return (
      row.name.toLowerCase().includes(q) ||
      row.channelName.toLowerCase().includes(q) ||
      row.aliases.some((item) => item.alias.toLowerCase().includes(q)) ||
      row.outbound.some((item) => item.alias.toLowerCase().includes(q))
    );
  });
  return sortInventory(rows);
});

const statusOptions = computed(() => {
  const unpriced = inventory.value.filter((row) => row.price === null).length;
  const priced = inventory.value.length - unpriced;
  return [
    { value: 'unpriced', label: t('models.unpriced'), count: unpriced },
    { value: 'priced', label: t('models.priced'), count: priced },
  ];
});

const channelOptions = computed(() => {
  const counts = new Map<string, number>();
  for (const row of inventory.value) {
    counts.set(row.channelName, (counts.get(row.channelName) ?? 0) + 1);
  }
  return [...(channelsQuery.data.value ?? [])]
    .map((channel) => ({
      value: channel.name,
      label: channel.name,
      count: counts.get(channel.name) ?? 0,
    }))
    .sort((left, right) => left.label.localeCompare(right.label));
});

const sections = computed(() => {
  const grouped = sectionInventory(filteredRows.value);
  const byName = new Map(grouped.map((section) => [section.channelName, section]));
  const selected = selectedChannels.value;
  const names =
    selected.length > 0
      ? [...selected].sort((left, right) => left.localeCompare(right))
      : grouped.map((section) => section.channelName);
  return names.map((name): InventorySection & { rows: InventorySectionRow[] } => {
    const section = byName.get(name) ?? { channelName: name, rows: [] };
    return {
      channelName: section.channelName,
      rows: section.rows.map((row) => ({
        ...row,
        aliasChipItems: aliasChips(row, section.channelName),
      })),
    };
  });
});

const selection = useRowSelection<string>();

const allVisibleSelected = computed({
  get: () =>
    filteredRows.value.length > 0 &&
    filteredRows.value.every((row) => selection.isSelected(inventoryRowKey(row))),
  set: (value) =>
    selection.setMany(
      filteredRows.value.map((row) => inventoryRowKey(row)),
      value,
    ),
});

const someVisibleSelected = computed(() =>
  filteredRows.value.some((row) => selection.isSelected(inventoryRowKey(row))),
);

watch(inventory, (rows) => selection.prune(rows.map((row) => inventoryRowKey(row))));

const selectedRows = computed(() =>
  inventory.value.filter((row) => selection.isSelected(inventoryRowKey(row))),
);

async function removeInventoryTargets(targets: InventoryDeleteTarget[]) {
  await apiClient.deleteChannelModels(
    targets.map((target) => ({ channel_id: target.channelId, model: target.name })),
  );
}

const deleteMutation = useMutation({
  mutationFn: (targets: InventoryDeleteTarget[]) => removeInventoryTargets(targets),
  onSuccess: async (_data, targets) => {
    const keys = new Set(
      targets.map((target) => inventoryRowKey({ channelId: target.channelId, name: target.name })),
    );
    for (const item of [...windows.value]) {
      if (item.payload.kind === 'bulk-delete') closeWindow(item.id, true);
      if (item.payload.kind === 'delete' && keys.has(inventoryRowKey(item.payload.row))) {
        closeWindow(item.id, true);
      }
    }
    selection.setMany([...keys], false);
    await invalidateChannelCaches(queryClient);
    await queryClient.invalidateQueries({ queryKey: ['prices'] });
  },
  onError: (err, targets) => {
    const message = extractApiError(err).message;
    error(message);
    const keys = new Set(
      targets.map((target) => inventoryRowKey({ channelId: target.channelId, name: target.name })),
    );
    for (const item of windows.value) {
      const matchDelete =
        item.payload.kind === 'delete' &&
        targets.length === 1 &&
        keys.has(inventoryRowKey(item.payload.row));
      if (item.payload.kind === 'bulk-delete' || matchDelete) {
        deleteErrors.value[item.id] = message;
      }
    }
  },
});

const deletePriceMutation = useMutation({
  mutationFn: ({ channelId, model }: { channelId: number; model: string }) =>
    apiClient.deletePrice(channelId, model),
  onSuccess: async (_data, target) => {
    for (const item of [...windows.value]) {
      if (
        item.payload.kind === 'delete-price' &&
        item.payload.row.channelId === target.channelId &&
        item.payload.row.name === target.model
      ) {
        closeWindow(item.id, true);
      }
    }
    await queryClient.invalidateQueries({ queryKey: ['prices'] });
    await queryClient.invalidateQueries({ queryKey: ['unified-models'] });
  },
  onError: (err, target) => {
    const message = extractApiError(err).message;
    error(message);
    for (const item of windows.value) {
      if (
        item.payload.kind === 'delete-price' &&
        item.payload.row.channelId === target.channelId &&
        item.payload.row.name === target.model
      ) {
        deleteErrors.value[item.id] = message;
      }
    }
  },
});

const deletingTarget = computed(() => {
  const targets = deleteMutation.variables.value;
  return deleteMutation.isPending.value && targets?.length === 1 ? (targets[0] ?? null) : null;
});

const bulkDeleting = computed(
  () => deleteMutation.isPending.value && (deleteMutation.variables.value?.length ?? 0) > 1,
);

function visibleDeleteTargets(): InventoryDeleteTarget[] {
  const targets: InventoryDeleteTarget[] = [];
  for (const section of sections.value) {
    for (const row of section.rows) {
      if (selection.isSelected(inventoryRowKey(row))) {
        targets.push({
          name: row.name,
          channelId: row.channelId,
          channelName: section.channelName,
        });
      }
    }
  }
  return targets;
}

function openEdit(row: InventoryRow) {
  const existing = windows.value.find(
    (entry) =>
      entry.payload.kind === 'editor' &&
      inventoryRowKey(entry.payload.row) === inventoryRowKey(row),
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'editor', row });
}

function openDelete(row: InventoryRow, channelName: string) {
  const existing = windows.value.find(
    (entry) =>
      entry.payload.kind === 'delete' &&
      entry.payload.row.name === row.name &&
      entry.payload.channelName === channelName,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  const entry = openWindow(takePendingAnchor(), { kind: 'delete', row, channelName });
  if (entry) deleteErrors.value[entry.id] = '';
}

function openDeletePrice(row: InventoryRow) {
  const existing = windows.value.find(
    (entry) =>
      entry.payload.kind === 'delete-price' &&
      entry.payload.row.channelId === row.channelId &&
      entry.payload.row.name === row.name,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  const entry = openWindow(takePendingAnchor(), { kind: 'delete-price', row });
  if (entry) deleteErrors.value[entry.id] = '';
}

function openBulkDelete() {
  const existing = windows.value.find((entry) => entry.payload.kind === 'bulk-delete');
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  const entry = openWindow(takePendingAnchor(), { kind: 'bulk-delete' });
  if (entry) deleteErrors.value[entry.id] = '';
}

/** 价格同步浮窗跟清单勾选实时同步，打开时不再快照行。 */
function openCatalog() {
  const existing = windows.value.find((entry) => entry.payload.kind === 'catalog');
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'catalog' });
}

/** 编辑价格「在线同步」：勾上当前行并打开/聚焦共用的价格同步浮窗。 */
function openCatalogForRow(row: InventoryRow) {
  const latest =
    inventory.value.find((item) => inventoryRowKey(item) === inventoryRowKey(row)) ?? row;
  selection.setMany([inventoryRowKey(latest)], true);
  openCatalog();
}

function formatOptionalAmount(value: number | null): string {
  return value === null ? t('common.emptyCell') : formatUsdAmount(value);
}

function refetchAll() {
  void channelsQuery.refetch();
  void pricesQuery.refetch();
}

function loadErrorMessage(): string {
  if (channelsQuery.isError.value) return extractApiError(channelsQuery.error.value).message;
  return extractApiError(pricesQuery.error.value).message;
}
</script>

<template>
  <div class="flex flex-col">
    <InlineError
      v-if="loadError && !channelsQuery.data.value && !pricesQuery.data.value"
      :message="loadErrorMessage()"
      @retry="refetchAll"
    />

    <div v-else class="flex flex-col">
      <DataTable
        class="[&_[data-slot=table]]:table-fixed"
        data-testid="inventory-table"
        :busy="showTableSkeleton"
      >
        <template #toolbar>
          <DataTableToolbar>
            <SearchInput
              id="inventory-search"
              v-model="searchText"
              class="max-w-sm"
              data-testid="inventory-search"
              :placeholder="t('models.search')"
              :aria-label="t('models.search')"
            />
            <FacetedFilter
              v-model="statusFilter"
              :title="t('models.statusFilter')"
              :options="statusOptions"
              test-id="inventory-status-filter"
            />
            <FacetedFilter
              v-model="selectedChannels"
              :title="t('models.channels')"
              :options="channelOptions"
              test-id="inventory-channel-filter"
            />
          </DataTableToolbar>
        </template>
        <!-- 复选框/价格/操作定宽；模型和别名不设宽，均分剩余，避免价格列吞掉身份列。 -->
        <colgroup>
          <col v-if="canSelectRows" class="w-10" />
          <col />
          <col />
          <col class="w-28" />
          <col class="w-28" />
          <col class="w-28" />
          <col class="w-28" />
          <col v-if="hasActions" class="w-24" />
        </colgroup>
        <TableHeader>
          <TableRow>
            <TableHead v-if="canSelectRows" class="w-10">
              <div class="flex items-center justify-center">
                <Checkbox
                  v-model="allVisibleSelected"
                  :indeterminate="someVisibleSelected && !allVisibleSelected"
                  data-testid="inventory-select-all"
                  :aria-label="t('common.selectAll')"
                />
              </div>
            </TableHead>
            <TableHead>{{ t('pricing.model') }}</TableHead>
            <TableHead>{{ t('models.alias') }}</TableHead>
            <TableHead>{{ t('pricing.inputUsd') }}</TableHead>
            <TableHead>{{ t('pricing.outputUsd') }}</TableHead>
            <TableHead>{{ t('pricing.cacheReadUsd') }}</TableHead>
            <TableHead>{{ t('pricing.cacheWriteUsd') }}</TableHead>
            <TableHead>{{ t('pricing.cacheWrite1hUsd') }}</TableHead>
            <TableHead v-if="hasActions" align="center" class="w-24">
              {{ t('common.actions') }}
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton
            v-if="showTableSkeleton"
            :has-select-column="canSelectRows"
            :columns="tableColumnCount"
          />
          <template v-else>
            <template v-for="section in sections" :key="section.channelName">
              <TableRow
                class="inventory-section-row"
                data-testid="inventory-section"
                :data-channel="section.channelName"
              >
                <TableCell :colspan="tableColumnCount" class="inventory-section-cell">
                  {{ section.channelName }}
                </TableCell>
              </TableRow>
              <TableRow
                v-for="row in section.rows"
                :key="inventoryRowKey(row)"
                data-testid="inventory-row"
                :data-model="row.name"
                :data-price-model="row.name"
                :data-section-channel="section.channelName"
                :data-unpriced="row.price === null ? 'true' : 'false'"
                :class="row.price === null ? 'inventory-row-unpriced' : undefined"
                :data-state="selection.isSelected(inventoryRowKey(row)) ? 'selected' : undefined"
              >
                <SelectCell
                  v-if="canSelectRows"
                  :checked="selection.isSelected(inventoryRowKey(row))"
                  test-id="inventory-select"
                  @toggle="selection.toggle(inventoryRowKey(row))"
                />
                <TableCell class="min-w-0 font-medium">
                  <span class="inline-flex min-w-0 items-center gap-2">
                    <CopyableName :text="row.name" test-id="inventory-model-name" />
                    <span
                      v-if="row.price === null"
                      class="badge badge-warn"
                      data-testid="inventory-unpriced"
                    >
                      {{ t('models.unpriced') }}
                    </span>
                  </span>
                </TableCell>
                <TableCell class="min-w-0" data-testid="inventory-alias">
                  <OverflowChips :items="row.aliasChipItems" chip-test-id="inventory-alias-chip" />
                </TableCell>
                <TableCell class="font-mono" data-testid="price-input">
                  {{ row.price ? formatUsdAmount(row.price.input_micros) : t('common.emptyCell') }}
                </TableCell>
                <TableCell class="font-mono" data-testid="price-output">
                  {{ row.price ? formatUsdAmount(row.price.output_micros) : t('common.emptyCell') }}
                </TableCell>
                <TableCell class="font-mono" data-testid="price-cache-read">
                  {{ row.price ? formatOptionalAmount(row.price.cache_read_micros) : '—' }}
                </TableCell>
                <TableCell class="font-mono" data-testid="price-cache-write">
                  {{ row.price ? formatOptionalAmount(row.price.cache_write_micros) : '—' }}
                </TableCell>
                <TableCell class="font-mono" data-testid="price-cache-write-1h">
                  {{ row.price ? formatOptionalAmount(row.price.cache_write_1h_micros) : '—' }}
                </TableCell>
                <TableCell v-if="hasActions" align="center">
                  <span class="inline-flex items-center justify-center gap-1">
                    <button
                      v-if="canEditPrices"
                      type="button"
                      class="btn btn-ghost btn-icon"
                      data-testid="pricing-edit-entry"
                      :aria-label="t('pricing.editPrice')"
                      :title="t('pricing.editPrice')"
                      @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                      @click="openEdit(row)"
                    >
                      <UiIcon name="pencil" :size="16" />
                    </button>
                    <DataTableRowActions
                      v-if="canRewriteChannels || (canEditPrices && row.price !== null)"
                    >
                      <DataTableMenuItem
                        v-if="canEditPrices && row.price !== null"
                        danger
                        data-testid="pricing-delete-entry"
                        @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                        @select="openDeletePrice(row)"
                      >
                        {{ t('pricing.removePrice') }}
                      </DataTableMenuItem>
                      <DataTableMenuSeparator
                        v-if="canRewriteChannels && canEditPrices && row.price !== null"
                      />
                      <DataTableMenuItem
                        v-if="canRewriteChannels"
                        danger
                        data-testid="inventory-delete"
                        @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                        @select="openDelete(row, section.channelName)"
                      >
                        {{ t('models.deleteModel') }}
                      </DataTableMenuItem>
                    </DataTableRowActions>
                  </span>
                </TableCell>
              </TableRow>
            </template>
            <TableRow v-if="sections.length === 0">
              <TableCell :colspan="tableColumnCount" class="h-24 whitespace-normal">
                <EmptyState :title="t('models.emptyInventory')" />
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
      <DataTableBulkBar
        v-if="canSelectRows"
        :count="selection.count.value"
        data-testid="inventory-bulk-bar"
        @clear="selection.clear"
      >
        <button
          v-if="canEditCatalog"
          type="button"
          class="btn bulk-bar__action"
          data-testid="inventory-bulk-catalog"
          @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
          @click="openCatalog"
        >
          {{ t('models.catalogFill') }}
        </button>
        <span
          v-if="canEditCatalog && canRewriteChannels"
          class="bulk-bar__divider"
          aria-hidden="true"
        />
        <button
          v-if="canRewriteChannels"
          type="button"
          class="btn btn-danger-filled bulk-bar__delete"
          data-testid="inventory-bulk-delete"
          @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
          @click="openBulkDelete"
        >
          {{ t('models.deleteModel') }}
        </button>
      </DataTableBulkBar>
    </div>

    <template v-for="(win, index) in windows" :key="win.id">
      <PriceEditorWindow
        v-if="win.payload.kind === 'editor'"
        :model="win.payload.row.name"
        :channel-id="win.payload.row.channelId"
        :channel-name="win.payload.row.channelName"
        :initial="win.payload.row.price"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :can-fill-from-catalog="canEditCatalog"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @catalog-sync="openCatalogForRow(win.payload.row)"
      />
      <CatalogFillWindow
        v-else-if="win.payload.kind === 'catalog'"
        :rows="selectedRows"
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
        :title="t('pricing.deleteTitle')"
        :message="
          t('pricing.deleteMessage', {
            name: win.payload.row.name,
            channel: win.payload.channelName,
          })
        "
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="deleteErrors[win.id] ?? ''"
        :busy="
          deletingTarget?.name === win.payload.row.name &&
          deletingTarget?.channelName === win.payload.channelName
        "
        confirm-test-id="inventory-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty, false)"
        @confirm="
          deleteMutation.mutate([
            {
              name: win.payload.row.name,
              channelId: win.payload.row.channelId,
              channelName: win.payload.channelName,
            },
          ])
        "
      />
      <ConfirmWindow
        v-else-if="win.payload.kind === 'delete-price'"
        :title="t('pricing.removePriceTitle')"
        :message="
          t('pricing.removePriceMessage', {
            name: win.payload.row.name,
            channel: win.payload.row.channelName,
          })
        "
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="deleteErrors[win.id] ?? ''"
        :busy="
          deletePriceMutation.isPending.value &&
          deletePriceMutation.variables.value?.channelId === win.payload.row.channelId &&
          deletePriceMutation.variables.value?.model === win.payload.row.name
        "
        :confirm-label="t('pricing.removePrice')"
        confirm-test-id="pricing-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty, false)"
        @confirm="
          deletePriceMutation.mutate({
            channelId: win.payload.row.channelId,
            model: win.payload.row.name,
          })
        "
      />
      <ConfirmWindow
        v-else
        :title="t('pricing.bulkDeleteTitle')"
        :message="t('pricing.bulkDeleteMessage', { count: selection.count.value })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="deleteErrors[win.id] ?? ''"
        :busy="bulkDeleting"
        confirm-test-id="inventory-bulk-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty, false)"
        @confirm="deleteMutation.mutate(visibleDeleteTargets())"
      />
    </template>
  </div>
</template>
