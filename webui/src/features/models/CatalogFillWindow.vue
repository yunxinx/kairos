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
import SearchInput from '@/components/ui/SearchInput.vue';
import SegmentSwitch, { type SegmentPair } from '@/components/ui/SegmentSwitch.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import CatalogBrowser from '@/features/models/CatalogBrowser.vue';
import {
  buildCatalogFillPreview,
  catalogSourceKey,
  type CatalogFillMode,
  type CatalogFillPreview,
  type CatalogPick,
} from '@/lib/catalog';
import { formatUsdAmount } from '@/lib/format';
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
const uid = useId();
const queryClient = useQueryClient();
const floatingWindow = useTemplateRef<FloatingWindowControls>('floatingWindow');
const picks = ref<Record<string, CatalogPick>>({});
const fillMode = ref<CatalogFillMode>('blanks');
const searchText = ref('');
const statusFilter = ref<string[]>([]);
const selectedChannels = ref<string[]>([]);
const writeError = ref('');
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
  [...new Set(props.rows.map((row) => row.channelName))]
    .sort((left, right) => left.localeCompare(right))
    .map((name) => ({ value: name, label: name })),
);

const statusOptions = computed(() => [
  { value: 'will-write', label: t('models.catalogWillWrite') },
  { value: 'no-match', label: t('models.catalogNoMatch') },
  { value: 'need-host', label: t('models.catalogNeedHost') },
  { value: 'unchanged', label: t('models.catalogSkipFilled') },
]);

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

function formatTier(value: number | null | undefined): string {
  if (value === null || value === undefined) return t('common.emptyCell');
  return formatUsdAmount(value);
}

/** 模型名/对照名省略；单价与状态列固定百分比，避免长名撑开。 */
const fillColumns = [
  { width: '20%' },
  { width: '14%' },
  { width: '16%' },
  { width: '10%' },
  { width: '10%' },
  { width: '10%' },
  { width: '10%' },
  { width: '10%' },
];

function previewOf(row: InventoryRow): CatalogFillPreview | undefined {
  return previewByKey.value.get(catalogSourceKey(row.channelId, row.name));
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

function pickHost(channelId: number, model: string, lookupId: string, providerId: string) {
  picks.value = {
    ...picks.value,
    [catalogSourceKey(channelId, model)]: { providerId, modelId: lookupId },
  };
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
          <SegmentSwitch
            v-model="fillMode"
            :options="modeOptions"
            :aria-label="t('models.catalogMode')"
          />
          <SearchInput
            :id="`catalog-fill-search-${uid}`"
            v-model="searchText"
            class="max-w-sm"
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
        </div>
        <InlineError
          v-if="catalogQuery.isError.value"
          :message="t('models.catalogError')"
          @retry="() => catalogQuery.refetch()"
        />
        <DataTablePanel v-else data-testid="catalog-preview">
          <div class="virtual-table-scroll seed-scrollbar max-h-96 overflow-auto">
            <table class="w-full table-fixed caption-bottom text-sm">
              <colgroup>
                <col
                  v-for="(column, index) in fillColumns"
                  :key="index"
                  :style="{ width: column.width }"
                />
              </colgroup>
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
                <TableRowsSkeleton v-if="catalogQuery.isPending.value" :columns="8" />
                <template v-else>
                  <template v-for="section in sections" :key="section.channelName">
                    <TableRow
                      class="inventory-section-row"
                      data-testid="catalog-preview-section"
                      :data-channel="section.channelName"
                    >
                      <TableCell :colspan="8" class="inventory-section-cell">
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
                      <TableCell truncate class="font-mono text-xs" :title="catalogLookupId(row)">{{
                        catalogLookupId(row)
                      }}</TableCell>
                      <TableCell>
                        <template v-if="previewOf(row)">
                          <UiSelect
                            v-if="previewOf(row)!.hits.length > 1"
                            :id="`catalog-host-${row.channelId}-${row.name}`"
                            :model-value="previewOf(row)!.selected?.providerId ?? ''"
                            :options="previewOf(row)!.hostOptions"
                            data-testid="catalog-host-select"
                            @update:model-value="
                              (value) =>
                                pickHost(row.channelId, row.name, catalogLookupId(row), value)
                            "
                          />
                          <span
                            v-else-if="previewOf(row)!.hits.length === 1"
                            class="inline-flex min-w-0 items-center gap-2"
                          >
                            <span class="truncate text-xs" :title="previewOf(row)!.hostName ?? ''">{{
                              previewOf(row)!.hostName
                            }}</span>
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
                            class="btn btn-ghost btn-sm"
                            data-testid="catalog-pick-from-dir"
                            @click="openBrowse(row)"
                          >
                            {{ t('models.catalogPickFromDir') }}
                          </button>
                        </template>
                      </TableCell>
                      <TableCell class="font-mono">{{
                        formatTier(previewOf(row)?.nextPrice?.input_micros)
                      }}</TableCell>
                      <TableCell class="font-mono">{{
                        formatTier(previewOf(row)?.nextPrice?.output_micros)
                      }}</TableCell>
                      <TableCell class="font-mono">
                        {{ formatTier(previewOf(row)?.nextPrice?.cache_read_micros) }}
                      </TableCell>
                      <TableCell class="font-mono">
                        {{ formatTier(previewOf(row)?.nextPrice?.cache_write_micros) }}
                      </TableCell>
                      <TableCell data-testid="catalog-preview-status">{{
                        previewOf(row) ? statusLabel(previewOf(row)!) : t('common.emptyCell')
                      }}</TableCell>
                    </TableRow>
                  </template>
                  <TableRow v-if="sections.length === 0">
                    <TableCell :colspan="8" class="h-24 whitespace-normal">
                      <EmptyState :title="t('models.catalogEmpty')" />
                    </TableCell>
                  </TableRow>
                </template>
              </TableBody>
            </table>
          </div>
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
    </template>
  </FloatingWindow>
</template>
