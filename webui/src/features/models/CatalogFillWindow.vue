<script setup lang="ts">
import { useId, computed, ref, useTemplateRef, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { Price } from '@/api/types';
import DataTablePanel from '@/components/ui/DataTablePanel.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FacetedFilter from '@/components/ui/FacetedFilter.vue';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import InlineError from '@/components/ui/InlineError.vue';
import { useToast } from '@/composables/useToast';
import SearchInput from '@/components/ui/SearchInput.vue';
import SegmentSwitch, { type SegmentPair } from '@/components/ui/SegmentSwitch.vue';
import Tooltip from '@/components/ui/Tooltip.vue';
import SplitTable from '@/components/ui/table/SplitTable.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import CatalogBrowser from '@/features/models/CatalogBrowser.vue';
import CatalogFillPriceCell from '@/features/models/CatalogFillPriceCell.vue';
import {
  buildCatalogFillPreview,
  catalogSourceKey,
  type CatalogFillMode,
  type CatalogFillPreview,
  type CatalogPick,
} from '@/lib/catalog';
import { countedFacetOptions } from '@/lib/faceted-filter';
import { catalogLookupId, sectionInventory, type InventoryRow } from '@/lib/inventory';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const props = withDefaults(
  defineProps<{
    /** 清单当前勾选行；打开后随勾选增减，不是打开瞬间的快照。 */
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

interface FloatingWindowControls {
  lockSize: () => void;
  unlockSize: () => void;
}

const { t } = useI18n();
const { error } = useToast();
const uid = useId();
const queryClient = useQueryClient();
const floatingWindow = useTemplateRef<FloatingWindowControls>('floatingWindow');
const picks = ref<Record<string, CatalogPick>>({});
const fillMode = ref<CatalogFillMode>('blanks');
const searchText = ref('');
const statusFilter = ref<string[]>([]);
const selectedChannels = ref<string[]>([]);
const editorView = ref<'preview' | 'browse'>('preview');
const browseKey = ref<string | null>(null);

const modeOptions = computed((): SegmentPair<CatalogFillMode> => [
  { value: 'blanks', label: t('models.catalogModeBlanks'), testId: 'catalog-mode-blanks' },
  {
    value: 'overwrite',
    label: t('models.catalogModeOverwrite'),
    testId: 'catalog-mode-overwrite',
  },
]);

const dirty = computed(() => Object.keys(picks.value).length > 0 || fillMode.value !== 'blanks');
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const catalogQuery = useQuery({
  queryKey: ['catalog'],
  queryFn: () => apiClient.getCatalog(),
});

const preview = computed((): CatalogFillPreview[] => {
  const catalog = catalogQuery.data.value?.models;
  if (!catalog) return [];
  return buildCatalogFillPreview(
    props.rows.map((row) => ({
      model: row.name,
      channelId: row.channelId,
      channelName: row.channelName,
      lookupId: catalogLookupId(row),
      price: row.price,
    })),
    catalog,
    picks.value,
    fillMode.value,
  );
});

const previewByKey = computed(() => {
  const map = new Map<string, CatalogFillPreview>();
  for (const row of preview.value) {
    map.set(catalogSourceKey(row.channelId, row.model), row);
  }
  return map;
});

const channelOptions = computed(() =>
  countedFacetOptions(props.rows.map((row) => row.channelName)),
);

const statusOptions = computed(() => {
  const counts = { 'will-write': 0, 'no-match': 0, 'need-host': 0, unchanged: 0 };
  for (const row of preview.value) {
    counts[row.status] += 1;
  }
  return [
    { value: 'will-write', label: t('models.catalogWillWrite'), count: counts['will-write'] },
    { value: 'no-match', label: t('models.catalogNoMatch'), count: counts['no-match'] },
    { value: 'need-host', label: t('models.catalogNeedHost'), count: counts['need-host'] },
    { value: 'unchanged', label: t('models.catalogSkipFilled'), count: counts.unchanged },
  ];
});

const filteredRows = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  const statuses = new Set(statusFilter.value);
  const channels = new Set(selectedChannels.value);
  return props.rows.filter((row) => {
    if (channels.size > 0 && !channels.has(row.channelName)) return false;
    const item = previewByKey.value.get(catalogSourceKey(row.channelId, row.name));
    if (statuses.size > 0 && (!item || !statuses.has(item.status))) return false;
    if (!q) return true;
    return (
      row.name.toLowerCase().includes(q) ||
      row.channelName.toLowerCase().includes(q) ||
      catalogLookupId(row).toLowerCase().includes(q)
    );
  });
});

const sections = computed(() => sectionInventory(filteredRows.value));

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

function statusBadgeClass(status: CatalogFillPreview['status']): string {
  if (status === 'will-write') return 'badge-success';
  if (status === 'need-host') return 'badge-warn';
  if (status === 'no-match') return 'badge-danger';
  return 'badge-neutral';
}

/** 模型名省略；单价列略宽以容纳覆盖删除线。 */
const fillColumns = [
  { width: '22%' },
  { width: '16%' },
  { width: '13%' },
  { width: '13%' },
  { width: '13%' },
  { width: '13%' },
  { width: '10%' },
];

function previewOf(row: InventoryRow): CatalogFillPreview | undefined {
  return previewByKey.value.get(catalogSourceKey(row.channelId, row.name));
}

function willOverwrite(row: InventoryRow): boolean {
  return fillMode.value === 'overwrite' && previewOf(row)?.status === 'will-write';
}

const writeMutation = useMutation({
  mutationFn: async (rows: CatalogFillPreview[]) => {
    for (const row of rows) {
      if (row.status !== 'will-write' || row.nextPrice === null) continue;
      const body: Price = row.nextPrice;
      const existing = props.rows.find(
        (item) => item.channelId === row.channelId && item.name === row.model,
      )?.price;
      if (existing) {
        await apiClient.updatePrice(row.channelId, row.model, body);
      } else {
        await apiClient.createPrice(body);
      }
    }
  },
  onSuccess: async () => {
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['prices'] });
    await queryClient.invalidateQueries({ queryKey: ['unified-models'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

function handleConfirm() {
  writeMutation.mutate(preview.value);
}

function openBrowse(row: InventoryRow) {
  floatingWindow.value?.lockSize();
  browseKey.value = catalogSourceKey(row.channelId, row.name);
  editorView.value = 'browse';
}

function leaveBrowse() {
  floatingWindow.value?.unlockSize();
  editorView.value = 'preview';
  browseKey.value = null;
}

watch(
  () => props.rows.map((row) => catalogSourceKey(row.channelId, row.name)),
  (keys) => {
    const alive = new Set(keys);
    const next: Record<string, CatalogPick> = {};
    for (const [key, pick] of Object.entries(picks.value)) {
      if (alive.has(key)) next[key] = pick;
    }
    if (Object.keys(next).length !== Object.keys(picks.value).length) {
      picks.value = next;
    }
    if (browseKey.value !== null && !alive.has(browseKey.value)) leaveBrowse();
  },
);

const browseInitialQuery = computed(() => {
  const key = browseKey.value;
  if (!key) return '';
  const row = props.rows.find((item) => catalogSourceKey(item.channelId, item.name) === key);
  return row ? catalogLookupId(row) : '';
});

function onCatalogPick(picked: { provider_id: string; model_id: string }) {
  const key = browseKey.value;
  if (!key) return;
  picks.value = {
    ...picks.value,
    [key]: { providerId: picked.provider_id, modelId: picked.model_id },
  };
  leaveBrowse();
}

function handleWindowClose() {
  if (editorView.value === 'browse') {
    leaveBrowse();
    return;
  }
  emit('close');
}
</script>

<template>
  <FloatingWindow
    ref="floatingWindow"
    extra-wide
    :title="editorView === 'browse' ? t('models.catalogBrowseTitle') : t('models.catalogTitle')"
    :anchor="anchor"
    :stack-order="stackOrder"
    :cascade="cascade"
    :attention="attention"
    :topmost="topmost"
    :close-aria-label="editorView === 'browse' ? t('models.catalogBrowseBack') : t('common.close')"
    @close="handleWindowClose"
    @pointerdown="emit('raise')"
  >
    <div v-if="editorView === 'browse'" class="card-body flex h-full min-h-0 flex-1 flex-col">
      <CatalogBrowser
        :initial-query="browseInitialQuery"
        @pick="onCatalogPick"
        @back="leaveBrowse"
      />
    </div>
    <template v-else>
      <div class="card-body space-y-3">
        <div class="flex flex-wrap items-center gap-2">
          <SearchInput
            :id="`catalog-fill-search-${uid}`"
            v-model="searchText"
            class="max-w-sm min-w-0"
            data-testid="catalog-search"
            :placeholder="t('models.search')"
            :aria-label="t('models.search')"
          />
          <FacetedFilter
            v-model="statusFilter"
            :title="t('models.statusFilter')"
            :options="statusOptions"
            test-id="catalog-status-filter"
          />
          <FacetedFilter
            v-model="selectedChannels"
            :title="t('models.channels')"
            :options="channelOptions"
            test-id="catalog-channel-filter"
          />
          <div class="ml-auto">
            <SegmentSwitch
              v-model="fillMode"
              :options="modeOptions"
              :aria-label="t('models.catalogMode')"
            />
          </div>
        </div>
        <InlineError
          v-if="catalogQuery.isError.value"
          :message="t('models.catalogError')"
          @retry="() => catalogQuery.refetch()"
        />
        <DataTablePanel v-else class="h-96" data-testid="catalog-preview">
          <SplitTable :columns="fillColumns" class="h-full">
            <template #header>
              <TableRow>
                <TableHead>{{ t('pricing.model') }}</TableHead>
                <TableHead>{{ t('models.catalogHost') }}</TableHead>
                <TableHead>{{ t('pricing.inputUsd') }}</TableHead>
                <TableHead>{{ t('pricing.outputUsd') }}</TableHead>
                <TableHead>{{ t('pricing.cacheReadUsd') }}</TableHead>
                <TableHead>{{ t('pricing.cacheWriteUsd') }}</TableHead>
                <TableHead>{{ t('channel.status') }}</TableHead>
              </TableRow>
            </template>
            <TableRowsSkeleton v-if="catalogQuery.isPending.value" :columns="7" />
            <template v-else>
              <template v-for="section in sections" :key="section.channelName">
                <TableRow
                  class="inventory-section-row"
                  data-testid="catalog-preview-section"
                  :data-channel="section.channelName"
                >
                  <TableCell :colspan="7" class="inventory-section-cell">
                    {{ t('models.sectionChannel', { name: section.channelName }) }}
                  </TableCell>
                </TableRow>
                <TableRow
                  v-for="row in section.rows"
                  :key="catalogSourceKey(row.channelId, row.name)"
                  data-testid="catalog-preview-row"
                  :data-model="row.name"
                  :data-channel="row.channelName"
                >
                  <TableCell truncate class="font-medium" :title="row.name">{{
                    row.name
                  }}</TableCell>
                  <TableCell>
                    <span
                      v-if="previewOf(row)?.selected"
                      class="inline-flex min-w-0 items-center gap-1"
                    >
                      <Tooltip :text="previewOf(row)!.hostName ?? ''">
                        <span
                          class="badge badge-info max-w-[9rem] truncate"
                          data-testid="catalog-host-name"
                          >{{ previewOf(row)!.hostName }}</span
                        >
                      </Tooltip>
                      <button
                        type="button"
                        class="btn btn-ghost btn-sm shrink-0"
                        data-testid="catalog-change-host"
                        @click="openBrowse(row)"
                      >
                        {{ t('models.catalogChangeHost') }}
                      </button>
                    </span>
                    <button
                      v-else
                      type="button"
                      class="btn btn-sm"
                      data-testid="catalog-pick-from-dir"
                      @click="openBrowse(row)"
                    >
                      {{ t('models.catalogPickFromDir') }}
                    </button>
                  </TableCell>
                  <TableCell class="font-mono">
                    <CatalogFillPriceCell
                      :current="row.price?.input_micros"
                      :next="previewOf(row)?.nextPrice?.input_micros"
                      :overwrite="willOverwrite(row)"
                    />
                  </TableCell>
                  <TableCell class="font-mono">
                    <CatalogFillPriceCell
                      :current="row.price?.output_micros"
                      :next="previewOf(row)?.nextPrice?.output_micros"
                      :overwrite="willOverwrite(row)"
                    />
                  </TableCell>
                  <TableCell class="font-mono">
                    <CatalogFillPriceCell
                      :current="row.price?.cache_read_micros"
                      :next="previewOf(row)?.nextPrice?.cache_read_micros"
                      :overwrite="willOverwrite(row)"
                    />
                  </TableCell>
                  <TableCell class="font-mono">
                    <CatalogFillPriceCell
                      :current="row.price?.cache_write_micros"
                      :next="previewOf(row)?.nextPrice?.cache_write_micros"
                      :overwrite="willOverwrite(row)"
                    />
                  </TableCell>
                  <TableCell data-testid="catalog-preview-status">
                    <span
                      v-if="previewOf(row)"
                      class="badge"
                      :class="statusBadgeClass(previewOf(row)!.status)"
                      >{{ statusLabel(previewOf(row)!) }}</span
                    >
                    <template v-else>{{ t('common.emptyCell') }}</template>
                  </TableCell>
                </TableRow>
              </template>
              <TableRow v-if="sections.length === 0">
                <TableCell :colspan="7" class="h-24 whitespace-normal">
                  <EmptyState :title="t('models.catalogEmpty')" />
                </TableCell>
              </TableRow>
            </template>
          </SplitTable>
        </DataTablePanel>
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
    </template>
  </FloatingWindow>
</template>
