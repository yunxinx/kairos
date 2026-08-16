<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { Price } from '@/api/types';
import Checkbox from '@/components/ui/Checkbox.vue';
import ConfirmWindow from '@/components/ui/ConfirmWindow.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import InlineError from '@/components/ui/InlineError.vue';
import SegmentSwitch, { type SegmentPair } from '@/components/ui/SegmentSwitch.vue';
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
import { useBulkDelete, type BulkDeletePayload } from '@/composables/useBulkDelete';
import { useRowSelection } from '@/composables/useRowSelection';
import { useWindowStack } from '@/composables/useWindowStack';
import CatalogFillWindow from '@/features/models/CatalogFillWindow.vue';
import PriceEditorWindow from '@/features/models/PriceEditorWindow.vue';
import { formatUsdMicros } from '@/lib/format';
import {
  buildInventory,
  sectionInventory,
  sortInventory,
  type InventoryLayout,
  type InventoryRow,
} from '@/lib/inventory';
import { anchorFromEvent, type FloatingWindowAnchor } from '@/lib/window-anchor';

type InventoryWindowPayload =
  | { kind: 'editor'; row: InventoryRow }
  | { kind: 'delete'; row: InventoryRow }
  | { kind: 'catalog'; rows: InventoryRow[] }
  | BulkDeletePayload;

const { t } = useI18n();
const queryClient = useQueryClient();

const searchText = ref('');
const layout = ref<InventoryLayout>('unified');
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
} = useWindowStack<InventoryWindowPayload>();

const deleteErrors = ref<Record<number, string>>({});

const channelsQuery = useQuery({
  queryKey: ['channels'],
  queryFn: () => apiClient.listChannels(),
});
const pricesQuery = useQuery({
  queryKey: ['prices'],
  queryFn: () => apiClient.listPrices(),
});

const loadError = computed(() => channelsQuery.isError.value || pricesQuery.isError.value);
const showTableSkeleton = computed(
  () =>
    (channelsQuery.isPending.value && !channelsQuery.data.value) ||
    (pricesQuery.isPending.value && !pricesQuery.data.value),
);

const inventory = computed(() =>
  buildInventory(channelsQuery.data.value ?? [], pricesQuery.data.value ?? []),
);

const filteredRows = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  const rows = q
    ? inventory.value.filter(
        (row) =>
          row.name.toLowerCase().includes(q) ||
          row.channelNames.some((name) => name.toLowerCase().includes(q)) ||
          row.aliases.some((alias) => alias.canonical.toLowerCase().includes(q)),
      )
    : inventory.value;
  return sortInventory(rows);
});

const sections = computed(() => sectionInventory(filteredRows.value, layout.value));

const layoutOptions = computed((): SegmentPair<InventoryLayout> => [
  {
    value: 'unified',
    label: t('models.layoutUnified'),
    testId: 'inventory-layout-unified',
  },
  {
    value: 'by-channel',
    label: t('models.layoutByChannel'),
    testId: 'inventory-layout-by-channel',
  },
]);

const selection = useRowSelection<string>();

const allVisibleSelected = computed({
  get: () =>
    filteredRows.value.length > 0 &&
    filteredRows.value.every((row) => selection.isSelected(row.name)),
  set: (value) =>
    selection.setMany(
      filteredRows.value.map((row) => row.name),
      value,
    ),
});

const someVisibleSelected = computed(() =>
  filteredRows.value.some((row) => selection.isSelected(row.name)),
);

watch(inventory, (rows) => selection.prune(rows.map((row) => row.name)));

const selectedRows = computed(() =>
  inventory.value.filter((row) => selection.isSelected(row.name)),
);

const pricedSelected = computed(() => selectedRows.value.filter((row) => row.price !== null));

const bulkDelete = useBulkDelete<string>({
  selection,
  windowStack: { windows, close: closeWindow },
  queryKey: ['prices'],
  deleteOne: (model) => apiClient.deletePrice(model),
});

const deleteMutation = useMutation({
  mutationFn: (model: string) => apiClient.deletePrice(model),
  onSuccess: async (_data, model) => {
    const entry = windows.value.find(
      (item) => item.payload.kind === 'delete' && item.payload.row.name === model,
    );
    if (entry) closeWindow(entry.id);
    await queryClient.invalidateQueries({ queryKey: ['prices'] });
  },
  onError: (err, model) => {
    const entry = windows.value.find(
      (item) => item.payload.kind === 'delete' && item.payload.row.name === model,
    );
    if (entry) deleteErrors.value[entry.id] = extractApiError(err).message;
  },
});

const deletingModel = computed(() =>
  deleteMutation.isPending.value ? (deleteMutation.variables.value ?? null) : null,
);

function openEdit(row: InventoryRow) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'editor' && entry.payload.row.name === row.name,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'editor', row });
}

function openDelete(row: InventoryRow) {
  if (row.price === null) return;
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'delete' && entry.payload.row.name === row.name,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  const entry = openWindow(takePendingAnchor(), { kind: 'delete', row });
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

function openCatalog() {
  const existing = windows.value.find((entry) => entry.payload.kind === 'catalog');
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'catalog', rows: selectedRows.value });
}

function formatOptionalMicros(value: number | null): string {
  return value === null ? '—' : formatUsdMicros(value);
}

function aliasLabel(row: InventoryRow): string {
  return row.aliases
    .map((alias) => `${row.name} → ${alias.canonical} (${alias.channelName})`)
    .join(', ');
}

function refetchAll() {
  void channelsQuery.refetch();
  void pricesQuery.refetch();
}

function loadErrorMessage(): string {
  if (channelsQuery.isError.value) return extractApiError(channelsQuery.error.value).message;
  return extractApiError(pricesQuery.error.value).message;
}

function bulkDeleteKeys(): string[] {
  return pricedSelected.value
    .map((row) => row.price)
    .filter((price): price is Price => price !== null)
    .map((price) => price.model);
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
      <DataTable :busy="showTableSkeleton">
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
            <SegmentSwitch
              v-model="layout"
              :options="layoutOptions"
              :aria-label="t('models.layoutLabel')"
            />
            <template #actions>
              <button
                type="button"
                class="btn btn-primary"
                data-testid="inventory-catalog-fill"
                :disabled="selection.count.value === 0"
                @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                @click="openCatalog"
              >
                {{ t('models.catalogFill') }}
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
                  data-testid="inventory-select-all"
                  :aria-label="t('common.selectAll')"
                />
              </div>
            </TableHead>
            <TableHead>{{ t('pricing.model') }}</TableHead>
            <TableHead>{{ t('models.channels') }}</TableHead>
            <TableHead>{{ t('models.alias') }}</TableHead>
            <TableHead>{{ t('pricing.input') }}</TableHead>
            <TableHead>{{ t('pricing.output') }}</TableHead>
            <TableHead>{{ t('pricing.cacheRead') }}</TableHead>
            <TableHead>{{ t('pricing.cacheWrite') }}</TableHead>
            <TableHead align="center">{{ t('common.actions') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" :columns="9" />
          <template v-else>
            <template v-for="section in sections" :key="section.channelName ?? 'all'">
              <TableRow
                v-if="section.channelName !== null"
                data-testid="inventory-section"
                :data-channel="section.channelName"
              >
                <TableCell :colspan="9" class="text-fg-muted bg-surface-alt text-xs font-medium">
                  {{ t('models.sectionChannel', { name: section.channelName }) }}
                </TableCell>
              </TableRow>
              <TableRow
                v-for="row in section.rows"
                :key="row.name"
                data-testid="inventory-row"
                :data-model="row.name"
                :data-price-model="row.name"
                :data-section-channel="section.channelName ?? undefined"
                :data-unpriced="row.price === null ? 'true' : 'false'"
                :class="row.price === null ? 'inventory-row-unpriced' : undefined"
                :data-state="selection.isSelected(row.name) ? 'selected' : undefined"
              >
                <SelectCell
                  :checked="selection.isSelected(row.name)"
                  test-id="inventory-select"
                  @toggle="selection.toggle(row.name)"
                />
                <TableCell class="font-medium">
                  <span class="inline-flex items-center gap-2">
                    {{ row.name }}
                    <span
                      v-if="row.price === null"
                      class="badge badge-warn"
                      data-testid="inventory-unpriced"
                    >
                      {{ t('models.unpriced') }}
                    </span>
                  </span>
                </TableCell>
                <TableCell>
                  <span class="flex flex-wrap gap-1">
                    <span
                      v-for="channelName in row.channelNames"
                      :key="channelName"
                      class="badge badge-info"
                      data-testid="inventory-channel-chip"
                    >
                      {{ channelName }}
                    </span>
                  </span>
                </TableCell>
                <TableCell class="text-sm" data-testid="inventory-alias">
                  {{ aliasLabel(row) || '—' }}
                </TableCell>
                <TableCell class="font-mono" data-testid="price-input">
                  {{ row.price ? formatUsdMicros(row.price.input_micros) : '—' }}
                </TableCell>
                <TableCell class="font-mono" data-testid="price-output">
                  {{ row.price ? formatUsdMicros(row.price.output_micros) : '—' }}
                </TableCell>
                <TableCell class="font-mono" data-testid="price-cache-read">
                  {{ row.price ? formatOptionalMicros(row.price.cache_read_micros) : '—' }}
                </TableCell>
                <TableCell class="font-mono" data-testid="price-cache-write">
                  {{ row.price ? formatOptionalMicros(row.price.cache_write_micros) : '—' }}
                </TableCell>
                <TableCell align="center">
                  <DataTableRowActions>
                    <DataTableMenuItem
                      data-testid="pricing-edit-entry"
                      @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                      @select="openEdit(row)"
                    >
                      {{ t('pricing.editPrice') }}
                    </DataTableMenuItem>
                    <template v-if="row.price !== null">
                      <DataTableMenuSeparator />
                      <DataTableMenuItem
                        danger
                        data-testid="pricing-delete-entry"
                        @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                        @select="openDelete(row)"
                      >
                        {{ t('common.delete') }}
                      </DataTableMenuItem>
                    </template>
                  </DataTableRowActions>
                </TableCell>
              </TableRow>
            </template>
            <TableRow v-if="filteredRows.length === 0">
              <TableCell :colspan="9" class="h-24 whitespace-normal">
                <EmptyState :title="t('models.emptyInventory')" />
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
      <DataTableBulkBar
        :count="selection.count.value"
        data-testid="inventory-bulk-bar"
        @clear="selection.clear"
      >
        <button
          type="button"
          class="btn"
          data-testid="inventory-bulk-catalog"
          @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
          @click="openCatalog"
        >
          {{ t('models.catalogFill') }}
        </button>
        <button
          v-if="pricedSelected.length > 0"
          type="button"
          class="btn btn-danger-filled bulk-bar__delete"
          data-testid="pricing-bulk-delete"
          @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
          @click="openBulkDelete"
        >
          {{ t('common.delete') }}
        </button>
      </DataTableBulkBar>
    </div>

    <template v-for="(win, index) in windows" :key="win.id">
      <PriceEditorWindow
        v-if="win.payload.kind === 'editor'"
        :model="win.payload.row.name"
        :initial="win.payload.row.price"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
      />
      <CatalogFillWindow
        v-else-if="win.payload.kind === 'catalog'"
        :rows="win.payload.rows"
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
        :message="t('pricing.deleteMessage', { name: win.payload.row.name })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="deleteErrors[win.id] ?? ''"
        :busy="deletingModel === win.payload.row.name"
        confirm-test-id="pricing-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="deleteMutation.mutate(win.payload.row.name)"
      />
      <ConfirmWindow
        v-else
        :title="t('pricing.bulkDeleteTitle')"
        :message="t('pricing.bulkDeleteMessage', { count: pricedSelected.length })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="bulkDelete.error.value"
        :busy="bulkDelete.isPending.value"
        confirm-test-id="pricing-bulk-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="bulkDelete.mutate(bulkDeleteKeys())"
      />
    </template>
  </div>
</template>
