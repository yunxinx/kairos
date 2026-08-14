<script setup lang="ts">
import { computed, ref } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { Price } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import ConfirmWindow from '@/components/ui/ConfirmWindow.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import InlineError from '@/components/ui/InlineError.vue';
import DataTable from '@/components/ui/data-table/DataTable.vue';
import DataTableMenuItem from '@/components/ui/data-table/DataTableMenuItem.vue';
import DataTableMenuSeparator from '@/components/ui/data-table/DataTableMenuSeparator.vue';
import DataTableRowActions from '@/components/ui/data-table/DataTableRowActions.vue';
import DataTableToolbar from '@/components/ui/data-table/DataTableToolbar.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import { useWindowStack } from '@/composables/useWindowStack';
import PriceEditorWindow from '@/features/pricing/PriceEditorWindow.vue';
import { formatUsdMicros } from '@/lib/format';
import { anchorFromEvent, type FloatingWindowAnchor } from '@/lib/window-anchor';

type PriceWindowPayload =
  { kind: 'editor'; price: Price | null } | { kind: 'delete'; price: Price };

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
            <FormTextInput
              id="pricing-search"
              v-model="searchText"
              type="text"
              class="h-8 max-w-xs"
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
            <TableHead>{{ t('pricing.model') }}</TableHead>
            <TableHead>{{ t('pricing.input') }}</TableHead>
            <TableHead>{{ t('pricing.output') }}</TableHead>
            <TableHead>{{ t('pricing.cacheRead') }}</TableHead>
            <TableHead>{{ t('pricing.cacheWrite') }}</TableHead>
            <TableHead align="center">{{ t('common.actions') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" :columns="6" />
          <template v-else>
            <TableRow
              v-for="price in filteredPrices"
              :key="price.model"
              data-testid="price-row"
              :data-price-model="price.model"
            >
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
              <TableCell :colspan="6" class="h-24 whitespace-normal">
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
        v-else
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
    </template>
  </div>
</template>
