<script setup lang="ts">
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { Price } from '@/api/types';
import Checkbox from '@/components/ui/Checkbox.vue';
import DataTablePanel from '@/components/ui/DataTablePanel.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import InlineError from '@/components/ui/InlineError.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import Table from '@/components/ui/table/Table.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import {
  CATALOG_TIERS,
  buildCatalogFillPreview,
  fetchModelsDevCatalog,
  type CatalogFillPreview,
  type CatalogTier,
} from '@/lib/catalog';
import { formatUsdAmount } from '@/lib/format';
import { catalogLookupId, type InventoryRow } from '@/lib/inventory';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const props = withDefaults(
  defineProps<{
    rows: InventoryRow[];
    anchor?: FloatingWindowAnchor | null;
    stackOrder?: number;
    cascade?: number;
    attention?: boolean;
    topmost?: boolean;
  }>(),
  { anchor: null, stackOrder: 0, cascade: 0, attention: false, topmost: true },
);

const emit = defineEmits<{
  close: [];
  raise: [];
  'dirty-change': [dirty: boolean];
}>();

const { t } = useI18n();
const uid = useId();
const queryClient = useQueryClient();
const hostPicks = ref<Record<string, string>>({});
const writeError = ref('');
const TIER_UI = [
  { id: 'input', testId: 'catalog-tier-input', labelKey: 'pricing.input' },
  { id: 'output', testId: 'catalog-tier-output', labelKey: 'pricing.output' },
  { id: 'cacheRead', testId: 'catalog-tier-cache-read', labelKey: 'pricing.cacheRead' },
  { id: 'cacheWrite', testId: 'catalog-tier-cache-write', labelKey: 'pricing.cacheWrite' },
] as const;
const tierFlags = ref<Record<CatalogTier, boolean>>(
  Object.fromEntries(CATALOG_TIERS.map((tier) => [tier, true])) as Record<CatalogTier, boolean>,
);

const selectedTiers = computed((): Set<CatalogTier> => {
  return new Set(CATALOG_TIERS.filter((tier) => tierFlags.value[tier]));
});

const dirty = computed(
  () =>
    Object.keys(hostPicks.value).length > 0 || CATALOG_TIERS.some((tier) => !tierFlags.value[tier]),
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const catalogQuery = useQuery({
  queryKey: ['models-dev-catalog'],
  queryFn: fetchModelsDevCatalog,
  staleTime: 60 * 60 * 1000,
});

const preview = computed((): CatalogFillPreview[] => {
  const catalog = catalogQuery.data.value;
  if (!catalog) return [];
  return buildCatalogFillPreview(
    props.rows.map((row) => ({
      model: row.name,
      lookupId: catalogLookupId(row),
      price: row.price,
    })),
    catalog,
    hostPicks.value,
    selectedTiers.value,
  );
});

const canConfirm = computed(
  () =>
    selectedTiers.value.size > 0 &&
    preview.value.some((row) => row.status === 'will-write') &&
    preview.value.every((row) => row.status !== 'need-host'),
);

function statusLabel(row: CatalogFillPreview): string {
  if (row.status === 'no-match') return t('models.catalogNoMatch');
  if (row.status === 'need-host') return t('models.catalogNeedHost');
  if (row.status === 'unchanged') return t('models.catalogSkipFilled');
  return t('models.catalogWillWrite');
}

function formatTier(value: number | null | undefined): string {
  if (value === null || value === undefined) return t('common.emptyCell');
  return formatUsdAmount(value);
}

const writeMutation = useMutation({
  mutationFn: async (rows: CatalogFillPreview[]) => {
    for (const row of rows) {
      if (row.status !== 'will-write' || row.nextPrice === null) continue;
      const body: Price = row.nextPrice;
      const existing = props.rows.find((item) => item.name === row.model)?.price;
      if (existing) {
        await apiClient.updatePrice(row.model, body);
      } else {
        await apiClient.createPrice(body);
      }
    }
  },
  onSuccess: async () => {
    writeError.value = '';
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['prices'] });
  },
  onError: (err) => {
    writeError.value = extractApiError(err).message;
  },
});

function handleConfirm() {
  writeError.value = '';
  writeMutation.mutate(preview.value);
}

function pickHost(model: string, providerId: string) {
  hostPicks.value = { ...hostPicks.value, [model]: providerId };
}
</script>

<template>
  <FloatingWindow
    :title="t('models.catalogTitle')"
    extra-wide
    :anchor="anchor"
    :stack-order="stackOrder"
    :cascade="cascade"
    :attention="attention"
    :topmost="topmost"
    @close="emit('close')"
    @pointerdown="emit('raise')"
  >
    <div class="card-body space-y-3">
      <fieldset>
        <legend class="form-field-label mb-2">{{ t('models.catalogTiers') }}</legend>
        <div class="flex flex-wrap gap-x-4 gap-y-2">
          <label
            v-for="tier in TIER_UI"
            :key="tier.id"
            class="inline-flex items-center gap-2 text-sm"
            :for="`catalog-tier-${tier.id}-${uid}`"
          >
            <Checkbox
              :id="`catalog-tier-${tier.id}-${uid}`"
              v-model="tierFlags[tier.id]"
              :data-testid="tier.testId"
            />
            {{ t(tier.labelKey) }}
          </label>
        </div>
      </fieldset>
      <InlineError
        v-if="catalogQuery.isError.value"
        :message="t('models.catalogError')"
        @retry="() => catalogQuery.refetch()"
      />
      <p v-else-if="catalogQuery.isPending.value" class="text-fg-muted text-sm">
        {{ t('common.loading') }}
      </p>
      <DataTablePanel v-else data-testid="catalog-preview">
        <Table class="min-w-max">
          <TableHeader>
            <TableRow>
              <TableHead>{{ t('pricing.model') }}</TableHead>
              <TableHead>{{ t('models.catalogLookup') }}</TableHead>
              <TableHead>{{ t('models.catalogHost') }}</TableHead>
              <TableHead>{{ t('pricing.inputUsd') }}</TableHead>
              <TableHead>{{ t('pricing.outputUsd') }}</TableHead>
              <TableHead>{{ t('pricing.cacheReadUsd') }}</TableHead>
              <TableHead>{{ t('pricing.cacheWriteUsd') }}</TableHead>
              <TableHead>{{ t('channel.status') }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow
              v-for="row in preview"
              :key="row.model"
              data-testid="catalog-preview-row"
              :data-model="row.model"
            >
              <TableCell class="font-medium">{{ row.model }}</TableCell>
              <TableCell class="font-mono text-xs">{{ row.lookupId }}</TableCell>
              <TableCell>
                <UiSelect
                  v-if="row.hits.length > 1"
                  :id="`catalog-host-${row.model}`"
                  :model-value="row.selectedProviderId ?? ''"
                  :options="row.hostOptions"
                  data-testid="catalog-host-select"
                  @update:model-value="(value) => pickHost(row.model, value)"
                />
                <span v-else-if="row.hostName" class="text-xs">{{ row.hostName }}</span>
                <span v-else class="text-fg-muted text-xs">{{ t('common.emptyCell') }}</span>
              </TableCell>
              <TableCell class="font-mono">{{ formatTier(row.nextPrice?.input_micros) }}</TableCell>
              <TableCell class="font-mono">{{
                formatTier(row.nextPrice?.output_micros)
              }}</TableCell>
              <TableCell class="font-mono">
                {{ formatTier(row.nextPrice?.cache_read_micros) }}
              </TableCell>
              <TableCell class="font-mono">
                {{ formatTier(row.nextPrice?.cache_write_micros) }}
              </TableCell>
              <TableCell data-testid="catalog-preview-status">{{ statusLabel(row) }}</TableCell>
            </TableRow>
            <TableRow v-if="preview.length === 0">
              <TableCell :colspan="8" class="h-24 whitespace-normal">
                <EmptyState :title="t('models.catalogEmpty')" />
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>
      </DataTablePanel>
      <p v-if="writeError" class="text-danger text-sm" data-testid="catalog-fill-error">
        {{ writeError }}
      </p>
    </div>
    <div class="card-footer card-body flex justify-between gap-2">
      <button type="button" class="btn" @click="emit('close')">
        {{ t('common.cancel') }}
      </button>
      <button
        type="button"
        class="btn btn-primary"
        data-testid="catalog-confirm"
        :disabled="!canConfirm || writeMutation.isPending.value || catalogQuery.isPending.value"
        @click="handleConfirm"
      >
        {{ t('common.confirm') }}
      </button>
    </div>
  </FloatingWindow>
</template>
