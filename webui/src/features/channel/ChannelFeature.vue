<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import { channelWriteBody, type Channel, type ChannelView } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import Checkbox from '@/components/ui/Checkbox.vue';
import ProtocolBadge from '@/components/ui/ProtocolBadge.vue';
import ConfirmWindow from '@/components/ui/ConfirmWindow.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FacetedFilter from '@/components/ui/FacetedFilter.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import InlineError from '@/components/ui/InlineError.vue';
import NumberStepper from '@/components/ui/NumberStepper.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
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
import { useToast } from '@/composables/useToast';
import ChannelEditorWindow from '@/features/channel/ChannelEditorWindow.vue';
import ChannelProbeWindow from '@/features/channel/ChannelProbeWindow.vue';
import OverflowChips from '@/components/ui/OverflowChips.vue';
import { listedModelChips } from '@/lib/model-list';
import { anchorFromEvent, type FloatingWindowAnchor } from '@/lib/window-anchor';

type ChannelWindowPayload =
  | { kind: 'editor'; channel: ChannelView | null }
  | { kind: 'delete'; channel: ChannelView }
  | { kind: 'probe'; channel: ChannelView }
  | BulkDeletePayload;

const { t } = useI18n();
const { error } = useToast();
const queryClient = useQueryClient();

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
} = useWindowStack<ChannelWindowPayload>();

const deleteErrors = ref<Record<number, string>>({});
const searchText = ref('');
const statusFilter = ref<string[]>([]);

const channelsQuery = useQuery({
  queryKey: ['channels'],
  queryFn: () => apiClient.listChannels(),
});

const channels = computed(() => channelsQuery.data.value ?? []);
const showTableSkeleton = computed(
  () => channelsQuery.isPending.value && !channelsQuery.data.value,
);

const statusOptions = computed(() => {
  const enabled = channels.value.filter((channel) => channel.enabled).length;
  return [
    { value: 'enabled', label: t('channel.statusEnabled'), count: enabled },
    {
      value: 'disabled',
      label: t('channel.statusDisabled'),
      count: channels.value.length - enabled,
    },
  ];
});

const filteredChannels = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  const statuses = new Set(statusFilter.value);
  return channels.value.filter((channel) => {
    if (statuses.size > 0) {
      const flag = channel.enabled ? 'enabled' : 'disabled';
      if (!statuses.has(flag)) return false;
    }
    if (!q) return true;
    if (channel.name.toLowerCase().includes(q)) return true;
    if (channel.base_url.toLowerCase().includes(q)) return true;
    if (channel.models.some((model) => model.toLowerCase().includes(q))) return true;
    return t(`protocol.${channel.protocol}`).toLowerCase().includes(q);
  });
});

// 行选择：全选只作用于当前可见行；被筛掉的已选行保留选择但不计入全选。
const selection = useRowSelection<string>();

const allVisibleSelected = computed({
  get: () =>
    filteredChannels.value.length > 0 &&
    filteredChannels.value.every((channel) => selection.isSelected(String(channel.id))),
  set: (value) =>
    selection.setMany(
      filteredChannels.value.map((channel) => String(channel.id)),
      value,
    ),
});

const someVisibleSelected = computed(() =>
  filteredChannels.value.some((channel) => selection.isSelected(String(channel.id))),
);

// 删除或刷新后列表键变化，剔除幽灵选择。
watch(channels, (rows) => selection.prune(rows.map((row) => String(row.id))));

const bulkDelete = useBulkDelete<string>({
  selection,
  windowStack: { windows, close: closeWindow },
  queryKey: ['channels'],
  deleteOne: (id) => apiClient.deleteChannel(Number(id)),
});

function invalidateChannels() {
  return queryClient.invalidateQueries({ queryKey: ['channels'] });
}

const deleteMutation = useMutation({
  mutationFn: (id: number) => apiClient.deleteChannel(id),
  onSuccess: async (_data, id) => {
    const entry = windows.value.find(
      (item) => item.payload.kind === 'delete' && item.payload.channel.id === id,
    );
    if (entry) closeWindow(entry.id);
    await invalidateChannels();
  },
  onError: (err, id) => {
    const message = extractApiError(err).message;
    error(message);
    const entry = windows.value.find(
      (item) => item.payload.kind === 'delete' && item.payload.channel.id === id,
    );
    if (entry) deleteErrors.value[entry.id] = message;
  },
});

const deletingId = computed(() =>
  deleteMutation.isPending.value ? (deleteMutation.variables.value ?? null) : null,
);

// 启用/禁用：整体替换写（PUT 携带完整定义），成功后重取列表。
const toggleMutation = useMutation({
  mutationFn: (channel: ChannelView) =>
    apiClient.updateChannel(channel.id, {
      ...channelWriteBody(channel),
      enabled: !channel.enabled,
    }),
  onSuccess: async () => {
    await invalidateChannels();
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

const togglingId = computed(() =>
  toggleMutation.isPending.value ? (toggleMutation.variables.value?.id ?? null) : null,
);

// 优先级/权重行内编辑：整体替换写（PUT 携带完整定义），成功后重取列表。
type ChannelFieldPatch = Partial<Pick<Channel, 'priority' | 'weight'>>;

const fieldMutation = useMutation({
  mutationFn: ({ channel, patch }: { channel: ChannelView; patch: ChannelFieldPatch }) =>
    apiClient.updateChannel(channel.id, { ...channelWriteBody(channel), ...patch }),
  onSuccess: async () => {
    await invalidateChannels();
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

const savingId = computed(() =>
  fieldMutation.isPending.value ? (fieldMutation.variables.value?.channel.id ?? null) : null,
);

function openCreate(event: Event) {
  openWindow(anchorFromEvent(event), { kind: 'editor', channel: null });
}

function modelListItems(channel: ChannelView) {
  return listedModelChips(channel.models, channel.model_aliases).map((chip) => ({
    name: chip.name,
    ...(chip.actualRequest !== undefined ? { actualRequest: chip.actualRequest } : {}),
    ...(chip.aliases.length > 0
      ? { tooltip: t('channel.chipCanonicalTooltip', { aliases: chip.aliases.join(', ') }) }
      : {}),
  }));
}

const channelModelChips = computed(() => {
  const map = new Map<number, ReturnType<typeof modelListItems>>();
  for (const channel of filteredChannels.value) {
    map.set(channel.id, modelListItems(channel));
  }
  return map;
});

function openEdit(channel: ChannelView) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'editor' && entry.payload.channel?.id === channel.id,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'editor', channel });
}

function openDelete(channel: ChannelView) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'delete' && entry.payload.channel.id === channel.id,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  const entry = openWindow(takePendingAnchor(), { kind: 'delete', channel });
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

function openProbe(channel: ChannelView) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'probe' && entry.payload.channel.id === channel.id,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'probe', channel });
}
</script>

<template>
  <div class="flex flex-col">
    <PageHeader :title="t('nav.channel')" />

    <InlineError
      v-if="channelsQuery.isError.value && !channelsQuery.data.value"
      :message="extractApiError(channelsQuery.error.value).message"
      @retry="() => channelsQuery.refetch()"
    />

    <div v-else class="flex flex-col">
      <DataTable :busy="showTableSkeleton">
        <template #toolbar>
          <DataTableToolbar>
            <SearchInput
              id="channels-search"
              v-model="searchText"
              class="max-w-sm"
              data-testid="channels-search"
              :placeholder="t('channel.search')"
              :aria-label="t('channel.search')"
            />
            <FacetedFilter
              v-model="statusFilter"
              :title="t('channel.status')"
              :options="statusOptions"
              test-id="channels-status-filter"
            />
            <template #actions>
              <button
                type="button"
                class="btn btn-primary"
                data-testid="create-channel"
                @click="openCreate"
              >
                {{ t('channel.create') }}
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
                  data-testid="channels-select-all"
                  :aria-label="t('common.selectAll')"
                />
              </div>
            </TableHead>
            <TableHead class="min-w-44">{{ t('channel.name') }}</TableHead>
            <TableHead>{{ t('channel.requestProtocol') }}</TableHead>
            <TableHead>{{ t('channel.models') }}</TableHead>
            <TableHead align="center" class="w-28 pr-1">{{ t('channel.priority') }}</TableHead>
            <TableHead align="center" class="w-28 pl-1">{{ t('channel.weight') }}</TableHead>
            <TableHead align="center">{{ t('channel.status') }}</TableHead>
            <TableHead align="center">{{ t('common.actions') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" has-select-column :columns="8" />
          <template v-else>
            <TableRow
              v-for="channel in filteredChannels"
              :key="channel.id"
              data-testid="channel-row"
              :data-channel-name="channel.name"
              :data-state="selection.isSelected(String(channel.id)) ? 'selected' : undefined"
            >
              <SelectCell
                :checked="selection.isSelected(String(channel.id))"
                test-id="channel-select"
                @toggle="selection.toggle(String(channel.id))"
              />
              <TableCell class="font-medium">{{ channel.name }}</TableCell>
              <TableCell>
                <ProtocolBadge :protocol="channel.protocol" />
              </TableCell>
              <TableCell data-testid="channel-models">
                <OverflowChips
                  :items="channelModelChips.get(channel.id) ?? []"
                  chip-test-id="channel-models-chip"
                />
              </TableCell>
              <TableCell align="center" class="pr-1">
                <NumberStepper
                  v-model="channel.priority"
                  data-testid="channel-priority-stepper"
                  :min="0"
                  :disabled="savingId === channel.id"
                  :label="t('channel.priority')"
                  @update:model-value="
                    (value) => fieldMutation.mutate({ channel, patch: { priority: value } })
                  "
                />
              </TableCell>
              <TableCell align="center" class="pl-1">
                <NumberStepper
                  v-model="channel.weight"
                  data-testid="channel-weight-stepper"
                  :min="1"
                  :disabled="savingId === channel.id"
                  :label="t('channel.weight')"
                  @update:model-value="
                    (value) => fieldMutation.mutate({ channel, patch: { weight: value } })
                  "
                />
              </TableCell>
              <TableCell align="center">
                <button
                  type="button"
                  class="badge cursor-pointer"
                  :class="channel.enabled ? 'badge-success' : 'badge-danger'"
                  data-testid="channel-toggle-enabled"
                  :disabled="togglingId === channel.id"
                  :aria-label="channel.enabled ? t('channel.disable') : t('channel.enable')"
                  :title="channel.enabled ? t('channel.disable') : t('channel.enable')"
                  @click="toggleMutation.mutate(channel)"
                >
                  {{ channel.enabled ? t('channel.statusEnabled') : t('channel.statusDisabled') }}
                </button>
              </TableCell>
              <TableCell align="center">
                <span class="inline-flex items-center justify-center gap-1">
                  <button
                    type="button"
                    class="btn btn-ghost btn-icon"
                    data-testid="channel-edit"
                    :aria-label="t('common.edit')"
                    :title="t('common.edit')"
                    @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                    @click="openEdit(channel)"
                  >
                    <UiIcon name="pencil" :size="16" />
                  </button>
                  <DataTableRowActions>
                    <DataTableMenuItem
                      data-testid="channel-test"
                      :disabled="channel.models.length === 0"
                      @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                      @select="openProbe(channel)"
                    >
                      {{ t('channel.test') }}
                    </DataTableMenuItem>
                    <DataTableMenuSeparator />
                    <DataTableMenuItem
                      danger
                      data-testid="channel-delete"
                      @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                      @select="openDelete(channel)"
                    >
                      {{ t('common.delete') }}
                    </DataTableMenuItem>
                  </DataTableRowActions>
                </span>
              </TableCell>
            </TableRow>
            <TableRow v-if="filteredChannels.length === 0">
              <TableCell :colspan="8" class="h-24 whitespace-normal">
                <EmptyState :title="t('common.emptyList')">
                  <button type="button" class="btn btn-primary" @click="openCreate">
                    {{ t('channel.create') }}
                  </button>
                </EmptyState>
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
      <DataTableBulkBar
        :count="selection.count.value"
        data-testid="channels-bulk-bar"
        @clear="selection.clear"
      >
        <button
          type="button"
          class="btn btn-danger-filled bulk-bar__delete"
          data-testid="channels-bulk-delete"
          @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
          @click="openBulkDelete"
        >
          {{ t('common.delete') }}
        </button>
      </DataTableBulkBar>
    </div>

    <template v-for="(win, index) in windows" :key="win.id">
      <ChannelEditorWindow
        v-if="win.payload.kind === 'editor'"
        :initial="win.payload.channel"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
      />
      <ChannelProbeWindow
        v-else-if="win.payload.kind === 'probe'"
        :channel="win.payload.channel"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
      />
      <ConfirmWindow
        v-else-if="win.payload.kind === 'delete'"
        :title="t('channel.deleteTitle')"
        :message="t('channel.deleteMessage', { name: win.payload.channel.name })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="deleteErrors[win.id] ?? ''"
        :busy="deletingId === win.payload.channel.id"
        confirm-test-id="channel-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="deleteMutation.mutate(win.payload.channel.id)"
      />
      <ConfirmWindow
        v-else
        :title="t('channel.bulkDeleteTitle')"
        :message="t('channel.bulkDeleteMessage', { count: selection.count.value })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="bulkDelete.error.value"
        :busy="bulkDelete.isPending.value"
        confirm-test-id="channel-bulk-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="bulkDelete.mutate([...selection.selected.value])"
      />
    </template>
  </div>
</template>
