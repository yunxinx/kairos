<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { Price } from '@/api/types';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import InlineError from '@/components/ui/InlineError.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import {
  buildCatalogFillPreview,
  fetchModelsDevCatalog,
  type CatalogFillPreview,
} from '@/lib/catalog';
import { formatUsdMicros } from '@/lib/format';
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
const queryClient = useQueryClient();
const hostPicks = ref<Record<string, string>>({});
const writeError = ref('');
watch(hostPicks, (picks) => emit('dirty-change', Object.keys(picks).length > 0), {
  immediate: true,
});

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
  );
});

const canConfirm = computed(
  () =>
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
  if (value === null || value === undefined) return '—';
  return formatUsdMicros(value);
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
    wide
    :anchor="anchor"
    :stack-order="stackOrder"
    :cascade="cascade"
    :attention="attention"
    :topmost="topmost"
    @close="emit('close')"
    @pointerdown="emit('raise')"
  >
    <div class="card-body space-y-3">
      <p class="text-fg-muted text-sm">{{ t('models.catalogLead') }}</p>
      <InlineError
        v-if="catalogQuery.isError.value"
        :message="t('models.catalogError')"
        @retry="() => catalogQuery.refetch()"
      />
      <p v-else-if="catalogQuery.isPending.value" class="text-fg-muted text-sm">
        {{ t('common.loading') }}
      </p>
      <div v-else class="overflow-x-auto" data-testid="catalog-preview">
        <table class="w-full text-sm">
          <thead>
            <tr class="text-fg-muted text-left">
              <th class="pr-3 pb-2 font-medium">{{ t('pricing.model') }}</th>
              <th class="pr-3 pb-2 font-medium">{{ t('models.catalogLookup') }}</th>
              <th class="pr-3 pb-2 font-medium">{{ t('models.catalogHost') }}</th>
              <th class="pr-3 pb-2 font-medium">{{ t('pricing.input') }}</th>
              <th class="pr-3 pb-2 font-medium">{{ t('pricing.output') }}</th>
              <th class="pr-3 pb-2 font-medium">{{ t('pricing.cacheRead') }}</th>
              <th class="pr-3 pb-2 font-medium">{{ t('pricing.cacheWrite') }}</th>
              <th class="pb-2 font-medium">{{ t('channel.status') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="row in preview"
              :key="row.model"
              :data-testid="'catalog-preview-row'"
              :data-model="row.model"
            >
              <td class="py-1 pr-3 font-medium">{{ row.model }}</td>
              <td class="py-1 pr-3 font-mono text-xs">{{ row.lookupId }}</td>
              <td class="py-1 pr-3">
                <UiSelect
                  v-if="row.hits.length > 1"
                  :id="`catalog-host-${row.model}`"
                  :model-value="row.selectedProviderId ?? ''"
                  :options="row.hostOptions"
                  data-testid="catalog-host-select"
                  @update:model-value="(value) => pickHost(row.model, value)"
                />
                <span v-else-if="row.hostName" class="text-xs">{{ row.hostName }}</span>
                <span v-else class="text-fg-muted text-xs">—</span>
              </td>
              <td class="py-1 pr-3 font-mono">{{ formatTier(row.nextPrice?.input_micros) }}</td>
              <td class="py-1 pr-3 font-mono">{{ formatTier(row.nextPrice?.output_micros) }}</td>
              <td class="py-1 pr-3 font-mono">
                {{ formatTier(row.nextPrice?.cache_read_micros) }}
              </td>
              <td class="py-1 pr-3 font-mono">
                {{ formatTier(row.nextPrice?.cache_write_micros) }}
              </td>
              <td class="py-1" data-testid="catalog-preview-status">{{ statusLabel(row) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
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
