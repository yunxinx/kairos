<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { ModelGroup } from '@/api/types';
import Checkbox from '@/components/ui/Checkbox.vue';
import ConfirmWindow from '@/components/ui/ConfirmWindow.vue';
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
import { useToast } from '@/composables/useToast';
import GroupEditorWindow from '@/features/models/GroupEditorWindow.vue';
import ModelSourceLines from '@/features/models/ModelSourceLines.vue';
import { groupModelDisplayLines } from '@/lib/group-models';
import { DEFAULT_MODEL_GROUP } from '@/lib/visible-models';
import { anchorFromEvent, type FloatingWindowAnchor } from '@/lib/window-anchor';

type GroupWindowPayload =
  | { kind: 'editor'; group: ModelGroup | null }
  | { kind: 'delete'; group: ModelGroup }
  | { kind: 'force-delete'; group: ModelGroup }
  | BulkDeletePayload;

const { t } = useI18n();
const { error } = useToast();
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
} = useWindowStack<GroupWindowPayload>();

const deleteErrors = ref<Record<number, string>>({});

const groupsQuery = useQuery({
  queryKey: ['model-groups'],
  queryFn: () => apiClient.listModelGroups(),
});
const unifiedQuery = useQuery({
  queryKey: ['unified-models'],
  queryFn: () => apiClient.listUnifiedModels(),
});
const channelsQuery = useQuery({
  queryKey: ['channels'],
  queryFn: () => apiClient.listChannels(),
});

const channels = computed(() => channelsQuery.data.value ?? []);
const unifiedModels = computed(() => unifiedQuery.data.value ?? []);

const groups = computed(() =>
  (groupsQuery.data.value ?? []).filter((group) => group.name !== DEFAULT_MODEL_GROUP),
);
const showTableSkeleton = computed(() => groupsQuery.isPending.value && !groupsQuery.data.value);

const memberLinesByGroup = computed(() => {
  const map = new Map<string, ReturnType<typeof groupModelDisplayLines>>();
  for (const group of groups.value) {
    map.set(group.name, groupModelDisplayLines(group.models, channels.value, unifiedModels.value));
  }
  return map;
});

const filtered = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  if (!q) return groups.value;
  return groups.value.filter((group) => {
    if (group.name.toLowerCase().includes(q)) return true;
    const lines = memberLinesByGroup.value.get(group.name) ?? [];
    return lines.some(
      (line) =>
        line.name.toLowerCase().includes(q) ||
        line.channels.some((channel) => channel.name.toLowerCase().includes(q)),
    );
  });
});

const selectableIds = computed(() => filtered.value.map((group) => group.name));

const selection = useRowSelection<string>();
const allVisibleSelected = computed({
  get: () =>
    selectableIds.value.length > 0 &&
    selectableIds.value.every((name) => selection.isSelected(name)),
  set: (value) => selection.setMany(selectableIds.value, value),
});
const someVisibleSelected = computed(() =>
  selectableIds.value.some((name) => selection.isSelected(name)),
);
watch(groups, (rows) => selection.prune(rows.map((row) => row.name)));

const bulkDelete = useBulkDelete<string>({
  selection,
  windowStack: { windows, close: closeWindow },
  queryKey: ['model-groups'],
  deleteOne: (name) => apiClient.deleteModelGroup(name),
});

const deleteMutation = useMutation({
  mutationFn: ({ name, force }: { name: string; force: boolean }) =>
    apiClient.deleteModelGroup(name, force),
  onSuccess: async (_data, { name }) => {
    const entry = windows.value.find((item) => {
      const payload = item.payload;
      return (
        (payload.kind === 'delete' || payload.kind === 'force-delete') &&
        payload.group.name === name
      );
    });
    if (entry) closeWindow(entry.id);
    await queryClient.invalidateQueries({ queryKey: ['model-groups'] });
    await queryClient.invalidateQueries({ queryKey: ['tokens'] });
    await queryClient.invalidateQueries({ queryKey: ['channels'] });
  },
  onError: (err, { name }) => {
    const { message, code } = extractApiError(err);
    const deleteEntry = windows.value.find((item) => item.payload.kind === 'delete');
    const payload = deleteEntry?.payload;
    if (
      deleteEntry &&
      payload?.kind === 'delete' &&
      payload.group.name === name &&
      code === 'conflict'
    ) {
      closeWindow(deleteEntry.id);
      openWindow(null, { kind: 'force-delete', group: payload.group });
      return;
    }
    const entry = windows.value.find((item) => {
      const itemPayload = item.payload;
      return (
        (itemPayload.kind === 'delete' || itemPayload.kind === 'force-delete') &&
        itemPayload.group.name === name
      );
    });
    if (entry) deleteErrors.value[entry.id] = message;
    error(message);
  },
});

const deletingName = computed(() =>
  deleteMutation.isPending.value ? (deleteMutation.variables.value?.name ?? null) : null,
);

function openCreate(event: Event) {
  openWindow(anchorFromEvent(event), { kind: 'editor', group: null });
}

function openEdit(group: ModelGroup) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'editor' && entry.payload.group?.name === group.name,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'editor', group });
}

function openDelete(group: ModelGroup) {
  if (group.name === DEFAULT_MODEL_GROUP) return;
  const existing = windows.value.find(
    (entry) =>
      (entry.payload.kind === 'delete' || entry.payload.kind === 'force-delete') &&
      entry.payload.group.name === group.name,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  const entry = openWindow(takePendingAnchor(), { kind: 'delete', group });
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
      v-if="groupsQuery.isError.value && !groupsQuery.data.value"
      :message="extractApiError(groupsQuery.error.value).message"
      @retry="() => groupsQuery.refetch()"
    />
    <div v-else class="flex flex-col">
      <DataTable class="[&_[data-slot=table]]:table-fixed" :busy="showTableSkeleton">
        <template #toolbar>
          <DataTableToolbar>
            <SearchInput
              id="groups-search"
              v-model="searchText"
              class="max-w-sm"
              data-testid="groups-search"
              :placeholder="t('models.search')"
              :aria-label="t('models.search')"
            />
            <template #actions>
              <button
                type="button"
                class="btn btn-primary"
                data-testid="group-create"
                @click="openCreate"
              >
                {{ t('models.groupCreate') }}
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
                  data-testid="groups-select-all"
                  :aria-label="t('common.selectAll')"
                />
              </div>
            </TableHead>
            <TableHead class="w-[28%]">{{ t('models.groupName') }}</TableHead>
            <TableHead class="w-[58%]">{{ t('models.groupMembers') }}</TableHead>
            <TableHead align="center" class="w-24">{{ t('common.actions') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" :columns="4" />
          <template v-else>
            <TableRow
              v-for="group in filtered"
              :key="group.name"
              data-testid="group-row"
              :data-group-name="group.name"
              :data-state="selection.isSelected(group.name) ? 'selected' : undefined"
            >
              <SelectCell
                :checked="selection.isSelected(group.name)"
                test-id="group-select"
                @toggle="selection.toggle(group.name)"
              />
              <TableCell class="font-mono font-medium" truncate :title="group.name">
                {{ group.name }}
              </TableCell>
              <TableCell class="whitespace-normal" data-testid="group-models">
                <ModelSourceLines
                  :lines="memberLinesByGroup.get(group.name) ?? []"
                  chip-test-id="group-source-channel"
                />
              </TableCell>
              <TableCell align="center">
                <span class="inline-flex items-center justify-center gap-1">
                  <button
                    type="button"
                    class="btn btn-ghost btn-icon"
                    data-testid="group-edit"
                    :aria-label="t('common.edit')"
                    :title="t('common.edit')"
                    @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                    @click="openEdit(group)"
                  >
                    <UiIcon name="pencil" :size="16" />
                  </button>
                  <DataTableRowActions>
                    <DataTableMenuItem
                      danger
                      data-testid="group-delete"
                      @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                      @select="openDelete(group)"
                    >
                      {{ t('common.delete') }}
                    </DataTableMenuItem>
                  </DataTableRowActions>
                </span>
              </TableCell>
            </TableRow>
            <TableRow v-if="filtered.length === 0">
              <TableCell :colspan="4" class="h-24 whitespace-normal">
                <EmptyState :title="t('models.groupEmpty')">
                  <button type="button" class="btn btn-primary" @click="openCreate">
                    {{ t('models.groupCreate') }}
                  </button>
                </EmptyState>
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
      <DataTableBulkBar
        :count="selection.count.value"
        data-testid="groups-bulk-bar"
        @clear="selection.clear"
      >
        <button
          type="button"
          class="btn btn-danger-filled bulk-bar__delete"
          data-testid="groups-bulk-delete"
          @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
          @click="openBulkDelete"
        >
          {{ t('common.delete') }}
        </button>
      </DataTableBulkBar>
    </div>

    <template v-for="(win, index) in windows" :key="win.id">
      <GroupEditorWindow
        v-if="win.payload.kind === 'editor'"
        :initial="win.payload.group"
        :channels="channelsQuery.data.value ?? []"
        :unified-models="unifiedQuery.data.value ?? []"
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
        :title="t('models.groupDeleteTitle')"
        :message="t('models.groupDeleteMessage', { name: win.payload.group.name })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="deleteErrors[win.id] ?? ''"
        :busy="deletingName === win.payload.group.name"
        confirm-test-id="group-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="deleteMutation.mutate({ name: win.payload.group.name, force: false })"
      />
      <ConfirmWindow
        v-else-if="win.payload.kind === 'force-delete'"
        :title="t('models.groupForceDeleteTitle')"
        :message="t('models.groupForceDeleteMessage', { name: win.payload.group.name })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="deleteErrors[win.id] ?? ''"
        :busy="deletingName === win.payload.group.name"
        confirm-test-id="group-force-delete-confirm"
        :confirm-label="t('common.forceDelete')"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="deleteMutation.mutate({ name: win.payload.group.name, force: true })"
      />
      <ConfirmWindow
        v-else
        :title="t('models.groupBulkDeleteTitle')"
        :message="t('models.groupBulkDeleteMessage', { count: selection.count.value })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="bulkDelete.error.value"
        :busy="bulkDelete.isPending.value"
        confirm-test-id="group-bulk-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="bulkDelete.mutate([...selection.selected.value])"
      />
    </template>
  </div>
</template>
