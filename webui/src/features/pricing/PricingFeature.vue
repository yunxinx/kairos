<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { Price } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import Checkbox from '@/components/ui/Checkbox.vue';
import ConfirmWindow from '@/components/ui/ConfirmWindow.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import InlineError from '@/components/ui/InlineError.vue';
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
import PriceEditorWindow from '@/features/pricing/PriceEditorWindow.vue';
import { formatUsdMicros } from '@/lib/format';
import { anchorFromEvent, type FloatingWindowAnchor } from '@/lib/window-anchor';

type PriceWindowPayload =
  { kind: 'editor'; price: Price | null } | { kind: 'delete'; price: Price } | BulkDeletePayload;

const { t } = useI18n();
const queryClient = useQueryClient();

const searchText = ref('');
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
} = useWindowStack<PriceWindowPayload>();

const deleteErrors = ref<Record<number, string>>({});

const pricesQuery = useQuery({
  queryKey: ['prices'],
  queryFn: () => apiClient.listPrices(),
});

const prices = computed(() => pricesQuery.data.value ?? []);
const showTableSkeleton = computed(() => pricesQuery.isPending.value && !pricesQuery.data.value);

const filteredPrices = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  if (!q) return prices.value;
  return prices.value.filter((price) => price.model.toLowerCase().includes(q));
});

// 行选择：全选只作用于当前可见行；被筛掉的已选行保留选择但不计入全选。
const selection = useRowSelection<string>();

const allVisibleSelected = computed({
  get: () =>
    filteredPrices.value.length > 0 &&
    filteredPrices.value.every((price) => selection.isSelected(price.model)),
  set: (value) =>
    selection.setMany(
      filteredPrices.value.map((price) => price.model),
      value,
    ),
});

const someVisibleSelected = computed(() =>
  filteredPrices.value.some((price) => selection.isSelected(price.model)),
);

// 删除或刷新后列表键变化，剔除幽灵选择。
watch(prices, (rows) => selection.prune(rows.map((row) => row.model)));

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
      (item) => item.payload.kind === 'delete' && item.payload.price.model === model,
    );
    if (entry) closeWindow(entry.id);
    await queryClient.invalidateQueries({ queryKey: ['prices'] });
  },
  onError: (err, model) => {
    const entry = windows.value.find(
      (item) => item.payload.kind === 'delete' && item.payload.price.model === model,
    );
    if (entry) deleteErrors.value[entry.id] = extractApiError(err).message;
  },
});

const deletingModel = computed(() =>
  deleteMutation.isPending.value ? (deleteMutation.variables.value ?? null) : null,
);

function openCreate(event: Event) {
  openWindow(anchorFromEvent(event), { kind: 'editor', price: null });
}

function openEdit(price: Price) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'editor' && entry.payload.price?.model === price.model,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'editor', price });
}

function openDelete(price: Price) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'delete' && entry.payload.price.model === price.model,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  const entry = openWindow(takePendingAnchor(), { kind: 'delete', price });
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

function formatOptionalMicros(value: number | null): string {
  return value === null ? '—' : formatUsdMicros(value);
}
</script>

<template>
  <div class="flex flex-col">
    <PageHeader :title="t('nav.pricing')" />

    <InlineError
      v-if="pricesQuery.isError.value && !pricesQuery.data.value"
      :message="extractApiError(pricesQuery.error.value).message"
      @retry="() => pricesQuery.refetch()"
    />

    <div v-else class="flex flex-col">
      <DataTable :busy="showTableSkeleton">
        <template #toolbar>
          <DataTableToolbar>
            <SearchInput
              id="pricing-search"
              v-model="searchText"
              class="max-w-sm"
              data-testid="pricing-search"
              :placeholder="t('pricing.search')"
              :aria-label="t('pricing.search')"
            />
            <template #actions>
              <button
                type="button"
                class="btn btn-primary"
                data-testid="pricing-create-entry"
                @click="openCreate"
              >
                {{ t('pricing.createEntry') }}
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
                  data-testid="pricing-select-all"
                  :aria-label="t('common.selectAll')"
                />
              </div>
            </TableHead>
            <TableHead>{{ t('pricing.model') }}</TableHead>
            <TableHead>{{ t('pricing.input') }}</TableHead>
            <TableHead>{{ t('pricing.output') }}</TableHead>
            <TableHead>{{ t('pricing.cacheRead') }}</TableHead>
            <TableHead>{{ t('pricing.cacheWrite') }}</TableHead>
            <TableHead align="center">{{ t('common.actions') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" :columns="7" />
          <template v-else>
            <TableRow
              v-for="price in filteredPrices"
              :key="price.model"
              data-testid="price-row"
              :data-price-model="price.model"
              :data-state="selection.isSelected(price.model) ? 'selected' : undefined"
            >
              <SelectCell
                :checked="selection.isSelected(price.model)"
                test-id="price-select"
                @toggle="selection.toggle(price.model)"
              />
              <TableCell class="font-medium">{{ price.model }}</TableCell>
              <TableCell class="font-mono" data-testid="price-input">
                {{ formatUsdMicros(price.input_micros) }}
              </TableCell>
              <TableCell class="font-mono" data-testid="price-output">
                {{ formatUsdMicros(price.output_micros) }}
              </TableCell>
              <TableCell class="font-mono" data-testid="price-cache-read">
                {{ formatOptionalMicros(price.cache_read_micros) }}
              </TableCell>
              <TableCell class="font-mono" data-testid="price-cache-write">
                {{ formatOptionalMicros(price.cache_write_micros) }}
              </TableCell>
              <TableCell align="center">
                <DataTableRowActions>
                  <DataTableMenuItem
                    data-testid="pricing-edit-entry"
                    @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                    @select="openEdit(price)"
                  >
                    {{ t('common.edit') }}
                  </DataTableMenuItem>
                  <DataTableMenuSeparator />
                  <DataTableMenuItem
                    danger
                    data-testid="pricing-delete-entry"
                    @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                    @select="openDelete(price)"
                  >
                    {{ t('common.delete') }}
                  </DataTableMenuItem>
                </DataTableRowActions>
              </TableCell>
            </TableRow>
            <TableRow v-if="filteredPrices.length === 0">
              <TableCell :colspan="7" class="h-24 whitespace-normal">
                <EmptyState :title="t('common.emptyList')">
                  <button type="button" class="btn btn-primary" @click="openCreate">
                    {{ t('pricing.createEntry') }}
                  </button>
                </EmptyState>
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
      <DataTableBulkBar
        :count="selection.count.value"
        data-testid="pricing-bulk-bar"
        @clear="selection.clear"
      >
        <button
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
        :initial="win.payload.price"
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
        :message="t('pricing.deleteMessage', { name: win.payload.price.model })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="deleteErrors[win.id] ?? ''"
        :busy="deletingModel === win.payload.price.model"
        confirm-test-id="pricing-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="deleteMutation.mutate(win.payload.price.model)"
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
        :error="bulkDelete.error.value"
        :busy="bulkDelete.isPending.value"
        confirm-test-id="pricing-bulk-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="bulkDelete.mutate([...selection.selected.value])"
      />
    </template>
  </div>
</template>
