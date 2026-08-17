<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { UnifiedModel } from '@/api/types';
import Checkbox from '@/components/ui/Checkbox.vue';
import ConfirmWindow from '@/components/ui/ConfirmWindow.vue';
import CopyableName from '@/components/ui/CopyableName.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import InlineError from '@/components/ui/InlineError.vue';
import DataTable from '@/components/ui/data-table/DataTable.vue';
import DataTableBulkBar from '@/components/ui/data-table/DataTableBulkBar.vue';
import DataTableMenuItem from '@/components/ui/data-table/DataTableMenuItem.vue';
import DataTableRowActions from '@/components/ui/data-table/DataTableRowActions.vue';
import DataTableToolbar from '@/components/ui/data-table/DataTableToolbar.vue';
import SelectCell from '@/components/ui/data-table/SelectCell.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import { useBulkDelete, type BulkDeletePayload } from '@/composables/useBulkDelete';
import { useRowSelection } from '@/composables/useRowSelection';
import { useWindowStack } from '@/composables/useWindowStack';
import UnifiedEditorWindow from '@/features/models/UnifiedEditorWindow.vue';
import { buildInventory } from '@/lib/inventory';
import { anchorFromEvent, type FloatingWindowAnchor } from '@/lib/window-anchor';

type UnifiedWindowPayload =
  | { kind: 'editor'; model: UnifiedModel | null }
  | { kind: 'delete'; model: UnifiedModel }
  | BulkDeletePayload;

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
} = useWindowStack<UnifiedWindowPayload>();

const deleteErrors = ref<Record<number, string>>({});

const unifiedQuery = useQuery({
  queryKey: ['unified-models'],
  queryFn: () => apiClient.listUnifiedModels(),
});
const channelsQuery = useQuery({
  queryKey: ['channels'],
  queryFn: () => apiClient.listChannels(),
});
const pricesQuery = useQuery({
  queryKey: ['prices'],
  queryFn: () => apiClient.listPrices(),
});

const memberOptions = computed(() => [
  ...new Set(
    buildInventory(channelsQuery.data.value ?? [], pricesQuery.data.value ?? []).map(
      (row) => row.name,
    ),
  ),
]);

const models = computed(() => unifiedQuery.data.value ?? []);
const showTableSkeleton = computed(() => unifiedQuery.isPending.value && !unifiedQuery.data.value);

const filtered = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  if (!q) return models.value;
  return models.value.filter(
    (model) =>
      model.id.toLowerCase().includes(q) ||
      model.models.some((member) => member.toLowerCase().includes(q)),
  );
});

const selection = useRowSelection<string>();
const allVisibleSelected = computed({
  get: () =>
    filtered.value.length > 0 && filtered.value.every((model) => selection.isSelected(model.id)),
  set: (value) =>
    selection.setMany(
      filtered.value.map((model) => model.id),
      value,
    ),
});
const someVisibleSelected = computed(() =>
  filtered.value.some((model) => selection.isSelected(model.id)),
);
watch(models, (rows) => selection.prune(rows.map((row) => row.id)));

const bulkDelete = useBulkDelete<string>({
  selection,
  windowStack: { windows, close: closeWindow },
  queryKey: ['unified-models'],
  deleteOne: (id) => apiClient.deleteUnifiedModel(id),
});

const deleteMutation = useMutation({
  mutationFn: (id: string) => apiClient.deleteUnifiedModel(id),
  onSuccess: async (_data, id) => {
    const entry = windows.value.find(
      (item) => item.payload.kind === 'delete' && item.payload.model.id === id,
    );
    if (entry) closeWindow(entry.id);
    await queryClient.invalidateQueries({ queryKey: ['unified-models'] });
  },
  onError: (err, id) => {
    const entry = windows.value.find(
      (item) => item.payload.kind === 'delete' && item.payload.model.id === id,
    );
    if (entry) deleteErrors.value[entry.id] = extractApiError(err).message;
  },
});

const deletingId = computed(() =>
  deleteMutation.isPending.value ? (deleteMutation.variables.value ?? null) : null,
);

function openCreate(event: Event) {
  openWindow(anchorFromEvent(event), { kind: 'editor', model: null });
}

function openEdit(model: UnifiedModel) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'editor' && entry.payload.model?.id === model.id,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'editor', model });
}

function openDelete(model: UnifiedModel) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'delete' && entry.payload.model.id === model.id,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  const entry = openWindow(takePendingAnchor(), { kind: 'delete', model });
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
</script>

<template>
  <div class="flex flex-col">
    <InlineError
      v-if="unifiedQuery.isError.value && !unifiedQuery.data.value"
      :message="extractApiError(unifiedQuery.error.value).message"
      @retry="() => unifiedQuery.refetch()"
    />
    <div v-else class="flex flex-col">
      <DataTable :busy="showTableSkeleton">
        <template #toolbar>
          <DataTableToolbar>
            <SearchInput
              id="unified-search"
              v-model="searchText"
              class="max-w-sm"
              data-testid="unified-search"
              :placeholder="t('models.search')"
              :aria-label="t('models.search')"
            />
            <template #actions>
              <button
                type="button"
                class="btn btn-primary"
                data-testid="unified-create"
                @click="openCreate"
              >
                {{ t('models.unifiedCreate') }}
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
                  data-testid="unified-select-all"
                  :aria-label="t('common.selectAll')"
                />
              </div>
            </TableHead>
            <TableHead>{{ t('models.unifiedId') }}</TableHead>
            <TableHead>{{ t('models.unifiedMembers') }}</TableHead>
            <TableHead>{{ t('models.unifiedHide') }}</TableHead>
            <TableHead align="center">{{ t('common.actions') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" :columns="5" />
          <template v-else>
            <TableRow
              v-for="model in filtered"
              :key="model.id"
              data-testid="unified-row"
              :data-unified-id="model.id"
              :data-state="selection.isSelected(model.id) ? 'selected' : undefined"
            >
              <SelectCell
                :checked="selection.isSelected(model.id)"
                test-id="unified-select"
                @toggle="selection.toggle(model.id)"
              />
              <TableCell class="font-mono font-medium">
                <CopyableName :text="model.id" test-id="unified-model-name" />
              </TableCell>
              <TableCell class="font-mono text-sm" data-testid="unified-members">
                {{ model.models.join(' → ') }}
              </TableCell>
              <TableCell>
                <span class="badge" :class="model.hide ? 'badge-warn' : 'badge-neutral'">
                  {{ model.hide ? t('models.unifiedHideOn') : t('models.unifiedHideOff') }}
                </span>
              </TableCell>
              <TableCell align="center">
                <span class="inline-flex items-center justify-center gap-1">
                  <button
                    type="button"
                    class="btn btn-ghost btn-icon"
                    data-testid="unified-edit"
                    :aria-label="t('common.edit')"
                    :title="t('common.edit')"
                    @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                    @click="openEdit(model)"
                  >
                    <UiIcon name="pencil" :size="16" />
                  </button>
                  <DataTableRowActions>
                    <DataTableMenuItem
                      danger
                      data-testid="unified-delete"
                      @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                      @select="openDelete(model)"
                    >
                      {{ t('common.delete') }}
                    </DataTableMenuItem>
                  </DataTableRowActions>
                </span>
              </TableCell>
            </TableRow>
            <TableRow v-if="filtered.length === 0">
              <TableCell :colspan="5" class="h-24 whitespace-normal">
                <EmptyState :title="t('models.unifiedEmpty')">
                  <button type="button" class="btn btn-primary" @click="openCreate">
                    {{ t('models.unifiedCreate') }}
                  </button>
                </EmptyState>
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
      <DataTableBulkBar
        :count="selection.count.value"
        data-testid="unified-bulk-bar"
        @clear="selection.clear"
      >
        <button
          type="button"
          class="btn btn-danger-filled bulk-bar__delete"
          data-testid="unified-bulk-delete"
          @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
          @click="openBulkDelete"
        >
          {{ t('common.delete') }}
        </button>
      </DataTableBulkBar>
    </div>

    <template v-for="(win, index) in windows" :key="win.id">
      <UnifiedEditorWindow
        v-if="win.payload.kind === 'editor'"
        :initial="win.payload.model"
        :member-options="memberOptions"
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
        :title="t('models.unifiedDeleteTitle')"
        :message="t('models.unifiedDeleteMessage', { name: win.payload.model.id })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="deleteErrors[win.id] ?? ''"
        :busy="deletingId === win.payload.model.id"
        confirm-test-id="unified-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="deleteMutation.mutate(win.payload.model.id)"
      />
      <ConfirmWindow
        v-else
        :title="t('models.unifiedBulkDeleteTitle')"
        :message="t('models.unifiedBulkDeleteMessage', { count: selection.count.value })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="bulkDelete.error.value"
        :busy="bulkDelete.isPending.value"
        confirm-test-id="unified-bulk-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="bulkDelete.mutate([...selection.selected.value])"
      />
    </template>
  </div>
</template>
