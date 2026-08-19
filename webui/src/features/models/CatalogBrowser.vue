<script setup lang="ts">
import { computed, ref, useId } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient } from '@/api/client';
import type { CatalogModel, CatalogQuery } from '@/api/types';
import DataTablePanel from '@/components/ui/DataTablePanel.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FacetedFilter from '@/components/ui/FacetedFilter.vue';
import InlineError from '@/components/ui/InlineError.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import Tooltip from '@/components/ui/Tooltip.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import VirtualTable from '@/components/ui/table/VirtualTable.vue';
import { catalogRowKey } from '@/lib/catalog';
import { formatUsdAmount } from '@/lib/format';

const props = withDefaults(
  defineProps<{
    /** 进入时预填的模型名搜索。 */
    initialQuery?: string;
  }>(),
  { initialQuery: '' },
);

const emit = defineEmits<{
  pick: [model: CatalogModel];
  back: [];
}>();

const { t } = useI18n();
const uid = useId();
const searchId = `catalog-browser-search-${uid}`;
const searchText = ref(props.initialQuery);
const selectedProviders = ref<string[]>([]);

const catalogParams = computed(() => {
  const q = searchText.value.trim();
  const providerId =
    selectedProviders.value.length > 0 ? [...selectedProviders.value].sort() : undefined;
  return {
    q: q || undefined,
    provider_id: providerId,
  };
});

const canBrowse = computed(
  () => Boolean(catalogParams.value.q) || Boolean(catalogParams.value.provider_id?.length),
);

const metaQuery = useQuery({
  queryKey: ['catalog-meta'],
  queryFn: () => apiClient.getCatalogMeta(),
});

const catalogQuery = useQuery({
  queryKey: computed(() => ['catalog', catalogParams.value] as const),
  queryFn: () => {
    const params: CatalogQuery = {};
    if (catalogParams.value.q) params.q = catalogParams.value.q;
    if (catalogParams.value.provider_id) params.provider_id = catalogParams.value.provider_id;
    return apiClient.getCatalog(params);
  },
  enabled: canBrowse,
});

const providerOptions = computed(() =>
  (metaQuery.data.value?.providers ?? []).map((provider) => ({
    value: provider.id,
    label: provider.name,
    count: provider.count,
  })),
);

const models = computed(() => catalogQuery.data.value?.models ?? []);

/** 百分比列宽：`auto` + truncate 会在窄窗把模型名列挤成 0。 */
const catalogColumns = [
  { width: '32%' },
  { width: '16%' },
  { width: '13%' },
  { width: '13%' },
  { width: '13%' },
  { width: '13%' },
];

function formatTier(value: number | null): string {
  if (value === null) return t('common.emptyCell');
  return formatUsdAmount(value);
}

function pickRow(model: CatalogModel) {
  emit('pick', model);
}

function onRowKeydown(model: CatalogModel, event: KeyboardEvent) {
  if (event.key === 'Enter' || event.key === ' ') {
    event.preventDefault();
    pickRow(model);
  }
}
</script>

<template>
  <div class="flex h-full min-h-0 flex-col gap-3" data-testid="catalog-browser">
    <div class="flex items-center gap-2">
      <button
        type="button"
        class="btn btn-sm inline-flex shrink-0 items-center gap-1"
        data-testid="catalog-browse-back"
        @click="emit('back')"
      >
        <UiIcon name="chevron-left" :size="14" />
        {{ t('models.catalogBrowseBackForm') }}
      </button>
      <SearchInput
        :id="searchId"
        v-model="searchText"
        class="min-w-0 flex-1"
        data-testid="catalog-browser-search"
        :placeholder="t('models.catalogBrowseSearch')"
        :aria-label="t('models.catalogBrowseSearch')"
      />
      <FacetedFilter
        v-model="selectedProviders"
        :title="t('models.catalogProviderFilter')"
        :options="providerOptions"
        test-id="catalog-provider-filter"
      />
    </div>
    <InlineError
      v-if="catalogQuery.isError.value"
      :message="t('models.catalogError')"
      @retry="() => catalogQuery.refetch()"
    />
    <DataTablePanel v-else class="flex min-h-0 flex-1 flex-col">
      <VirtualTable
        class="min-h-0 flex-1"
        :rows="canBrowse ? models : []"
        :colspan="6"
        :columns="catalogColumns"
        :loading="canBrowse && catalogQuery.isPending.value"
        :get-row-key="catalogRowKey"
        :empty-title="
          canBrowse ? t('models.catalogBrowseEmpty') : t('models.catalogBrowseNeedProvider')
        "
      >
        <template #header>
          <TableRow>
            <TableHead>{{ t('pricing.model') }}</TableHead>
            <TableHead>{{ t('models.catalogHost') }}</TableHead>
            <TableHead>{{ t('pricing.inputUsd') }}</TableHead>
            <TableHead>{{ t('pricing.outputUsd') }}</TableHead>
            <TableHead>{{ t('pricing.cacheReadUsd') }}</TableHead>
            <TableHead>{{ t('pricing.cacheWriteUsd') }}</TableHead>
          </TableRow>
        </template>
        <template #row="{ row: model }">
          <TableRow
            class="cursor-pointer"
            tabindex="0"
            data-testid="catalog-browser-row"
            :data-model="model.model_id"
            :data-provider="model.provider_id"
            :aria-label="t('models.catalogPick')"
            @click="pickRow(model)"
            @keydown="onRowKeydown(model, $event)"
          >
            <TableCell truncate class="font-mono text-sm" :title="model.model_id">{{
              model.model_id
            }}</TableCell>
            <TableCell class="max-w-0">
              <Tooltip :text="model.provider_name">
                <span class="badge badge-info max-w-full min-w-0 truncate">{{
                  model.provider_name
                }}</span>
              </Tooltip>
            </TableCell>
            <TableCell class="font-mono">{{ formatTier(model.input_micros) }}</TableCell>
            <TableCell class="font-mono">{{ formatTier(model.output_micros) }}</TableCell>
            <TableCell class="font-mono">{{ formatTier(model.cache_read_micros) }}</TableCell>
            <TableCell class="font-mono">{{ formatTier(model.cache_write_micros) }}</TableCell>
          </TableRow>
        </template>
        <template v-if="!canBrowse" #empty>
          <EmptyState :title="t('models.catalogBrowseNeedProvider')" />
        </template>
      </VirtualTable>
    </DataTablePanel>
  </div>
</template>
