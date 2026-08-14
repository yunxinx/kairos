<script setup lang="ts">
import { computed, ref } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { Channel, ChannelProbeResult } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import ConfirmWindow from '@/components/ui/ConfirmWindow.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
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
import ChannelEditorWindow from '@/features/channel/ChannelEditorWindow.vue';
import { anchorFromEvent, type FloatingWindowAnchor } from '@/lib/window-anchor';

type ChannelWindowPayload =
  { kind: 'editor'; channel: Channel | null } | { kind: 'delete'; channel: Channel };

const { t } = useI18n();
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

const actionError = ref('');
const deleteErrors = ref<Record<number, string>>({});
const probeByName = ref<Record<string, ChannelProbeResult>>({});
const testingName = ref<string | null>(null);
const searchText = ref('');

const channelsQuery = useQuery({
  queryKey: ['channels'],
  queryFn: () => apiClient.listChannels(),
});

const channels = computed(() => channelsQuery.data.value ?? []);
const showTableSkeleton = computed(
  () => channelsQuery.isPending.value && !channelsQuery.data.value,
);

const filteredChannels = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  if (!q) return channels.value;
  return channels.value.filter((channel) => {
    if (channel.name.toLowerCase().includes(q)) return true;
    if (channel.base_url.toLowerCase().includes(q)) return true;
    if (channel.models.some((model) => model.toLowerCase().includes(q))) return true;
    return t(`protocol.${channel.protocol}`).toLowerCase().includes(q);
  });
});

function invalidateChannels() {
  return queryClient.invalidateQueries({ queryKey: ['channels'] });
}

const deleteMutation = useMutation({
  mutationFn: (name: string) => apiClient.deleteChannel(name),
  onSuccess: async (_data, name) => {
    const entry = windows.value.find(
      (item) => item.payload.kind === 'delete' && item.payload.channel.name === name,
    );
    if (entry) closeWindow(entry.id);
    await invalidateChannels();
  },
  onError: (err, name) => {
    const entry = windows.value.find(
      (item) => item.payload.kind === 'delete' && item.payload.channel.name === name,
    );
    if (entry) deleteErrors.value[entry.id] = extractApiError(err).message;
  },
});

const deletingName = computed(() =>
  deleteMutation.isPending.value ? (deleteMutation.variables.value ?? null) : null,
);

const testMutation = useMutation({
  mutationFn: (name: string) => apiClient.testChannel(name),
  onSuccess: (result, name) => {
    probeByName.value = { ...probeByName.value, [name]: result };
    testingName.value = null;
  },
  onError: (err, name) => {
    testingName.value = null;
    actionError.value = `${name}: ${extractApiError(err).message}`;
  },
});

function openCreate(event: Event) {
  openWindow(anchorFromEvent(event), { kind: 'editor', channel: null });
}

function openEdit(channel: Channel) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'editor' && entry.payload.channel?.name === channel.name,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'editor', channel });
}

function openDelete(channel: Channel) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'delete' && entry.payload.channel.name === channel.name,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  const entry = openWindow(takePendingAnchor(), { kind: 'delete', channel });
  if (entry) deleteErrors.value[entry.id] = '';
}

function handleTest(name: string) {
  actionError.value = '';
  testingName.value = name;
  testMutation.mutate(name);
}

function probeText(result: ChannelProbeResult): string {
  if (!result.reachable) {
    return t('channel.probeUnreachable', { latency: result.latency_ms });
  }
  if (result.error) {
    return t('channel.probeFailure', {
      status: result.status_code ?? '—',
      latency: result.latency_ms,
    });
  }
  return t('channel.probeSuccess', {
    status: result.status_code ?? '—',
    latency: result.latency_ms,
  });
}

function probeClass(result: ChannelProbeResult): string {
  return result.reachable && result.error === null ? 'badge-success' : 'badge-danger';
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
      <p v-if="actionError" class="text-danger mb-4 shrink-0">{{ actionError }}</p>

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
            <TableHead>{{ t('channel.name') }}</TableHead>
            <TableHead>{{ t('channel.protocol') }}</TableHead>
            <TableHead>{{ t('channel.baseUrl') }}</TableHead>
            <TableHead>{{ t('channel.models') }}</TableHead>
            <TableHead align="center">{{ t('channel.priority') }}</TableHead>
            <TableHead align="center">{{ t('common.actions') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" :columns="6" />
          <template v-else>
            <TableRow
              v-for="channel in filteredChannels"
              :key="channel.name"
              data-testid="channel-row"
              :data-channel-name="channel.name"
            >
              <TableCell class="font-medium">{{ channel.name }}</TableCell>
              <TableCell>{{ t(`protocol.${channel.protocol}`) }}</TableCell>
              <TableCell class="font-mono text-sm">{{ channel.base_url }}</TableCell>
              <TableCell class="font-mono text-sm">
                {{ channel.models.join(', ') }}
              </TableCell>
              <TableCell align="center" class="font-mono">{{ channel.priority }}</TableCell>
              <TableCell align="center">
                <span class="inline-flex items-center justify-center gap-1">
                  <span
                    v-if="probeByName[channel.name]"
                    class="badge"
                    :class="probeClass(probeByName[channel.name]!)"
                    data-testid="channel-probe-result"
                  >
                    {{ probeText(probeByName[channel.name]!) }}
                  </span>
                  <DataTableRowActions>
                    <DataTableMenuItem
                      data-testid="channel-test"
                      :disabled="testingName === channel.name"
                      @select="handleTest(channel.name)"
                    >
                      {{ testingName === channel.name ? t('channel.testing') : t('channel.test') }}
                    </DataTableMenuItem>
                    <DataTableMenuSeparator />
                    <DataTableMenuItem
                      data-testid="channel-edit"
                      @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                      @select="openEdit(channel)"
                    >
                      {{ t('common.edit') }}
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
              <TableCell :colspan="6" class="h-24 whitespace-normal">
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
      <ConfirmWindow
        v-else
        :title="t('channel.deleteTitle')"
        :message="t('channel.deleteMessage', { name: win.payload.channel.name })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="deleteErrors[win.id] ?? ''"
        :busy="deletingName === win.payload.channel.name"
        confirm-test-id="channel-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="deleteMutation.mutate(win.payload.channel.name)"
      />
    </template>
  </div>
</template>
