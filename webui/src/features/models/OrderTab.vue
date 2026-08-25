<script setup lang="ts">
// 同名渠道顺序 Tab：只列出至少被两条渠道登记的可调用名，按运营拖拽调整渠道顺序。
// 每个名字一行，渠道顺序在本行内拖拽；保存时整体替换该名字的候选顺序。
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { ChannelModelOrder } from '@/api/types';
import CopyableName from '@/components/ui/CopyableName.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import InlineError from '@/components/ui/InlineError.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import DataTable from '@/components/ui/data-table/DataTable.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import { useChannelDirectory } from '@/composables/useChannelDirectory';
import { useToast } from '@/composables/useToast';
import { moveItem } from '@/lib/move-item';
import { useCurrentUser } from '@/lib/session';

const { t } = useI18n();
const { error } = useToast();
const queryClient = useQueryClient();
const me = useCurrentUser();
const canEditOrder = computed(() => me.value?.role === 'root');

const ordersQuery = useQuery({
  queryKey: ['channel-model-orders'],
  queryFn: () => apiClient.listChannelModelOrders(),
});
const { query: channelsQuery, channels } = useChannelDirectory();

const originalOrders = ref<ChannelModelOrder[]>([]);
const draftOrders = ref<ChannelModelOrder[]>([]);

watch(
  ordersQuery.data,
  (data) => {
    const next = (data ?? []).map((order) => ({
      model: order.model,
      channel_ids: [...order.channel_ids],
    }));
    originalOrders.value = next;
    draftOrders.value = next.map((order) => ({
      model: order.model,
      channel_ids: [...order.channel_ids],
    }));
  },
  { immediate: true },
);

const showTableSkeleton = computed(
  () =>
    (ordersQuery.isPending.value || channelsQuery.isPending.value) &&
    (!ordersQuery.data.value || !channelsQuery.data.value),
);

function channelName(channelId: number): string {
  return channels.value.find((channel) => channel.id === channelId)?.name ?? `#${channelId}`;
}

function channelEnabled(channelId: number): boolean | undefined {
  return channels.value.find((channel) => channel.id === channelId)?.enabled;
}

interface OrderRow extends ChannelModelOrder {
  channels: Array<{ id: number; name: string; enabled: boolean | undefined }>;
}

const rows = computed<OrderRow[]>(() =>
  draftOrders.value.map((order) => ({
    ...order,
    channels: order.channel_ids.map((id) => ({
      id,
      name: channelName(id),
      enabled: channelEnabled(id),
    })),
  })),
);

function sameOrder(left: ChannelModelOrder, right: ChannelModelOrder): boolean {
  return (
    left.model === right.model &&
    left.channel_ids.length === right.channel_ids.length &&
    left.channel_ids.every((id, index) => id === right.channel_ids[index])
  );
}

function isDirty(model: string): boolean {
  const current = draftOrders.value.find((order) => order.model === model);
  const original = originalOrders.value.find((order) => order.model === model);
  return current !== undefined && original !== undefined && !sameOrder(current, original);
}

function findOrder(model: string): ChannelModelOrder | undefined {
  return draftOrders.value.find((order) => order.model === model);
}

const dragModel = ref<string | null>(null);
const dragFrom = ref<number | null>(null);
const dropInsert = ref<number | null>(null);

function onChannelDragStart(model: string, index: number, event: DragEvent) {
  if (!canEditOrder.value) return;
  dragModel.value = model;
  dragFrom.value = index;
  event.dataTransfer?.setData('text/plain', `${model}\0${index}`);
  if (event.dataTransfer) event.dataTransfer.effectAllowed = 'move';
}

function onChannelDragOver(model: string, index: number, event: DragEvent) {
  if (!canEditOrder.value) return;
  if (dragModel.value !== model) return;
  event.preventDefault();
  if (event.dataTransfer) event.dataTransfer.dropEffect = 'move';
  const el = event.currentTarget as HTMLElement;
  const rect = el.getBoundingClientRect();
  dropInsert.value = event.clientY < rect.top + rect.height / 2 ? index : index + 1;
}

function onChannelDrop(model: string) {
  if (!canEditOrder.value) return;
  if (dragModel.value !== model || dragFrom.value === null || dropInsert.value === null) {
    resetDrag();
    return;
  }
  const order = findOrder(model);
  if (order) {
    const from = dragFrom.value;
    let insert = dropInsert.value;
    if (from < insert) insert -= 1;
    order.channel_ids = moveItem(order.channel_ids, from, insert);
  }
  resetDrag();
}

function onDragEnd() {
  resetDrag();
}

function resetDrag() {
  dragModel.value = null;
  dragFrom.value = null;
  dropInsert.value = null;
}

function channelDropClass(model: string, index: number): string {
  if (dragModel.value !== model) return '';
  if (dropInsert.value === index) return 'order-drop-before';
  if (dropInsert.value === index + 1 && index === (findOrder(model)?.channel_ids.length ?? 0) - 1) {
    return 'order-drop-after';
  }
  return '';
}

const saveMutation = useMutation({
  mutationFn: ({ model, channelIds }: { model: string; channelIds: number[] }) =>
    apiClient.replaceChannelModelOrder(model, channelIds),
  onSuccess: async () => {
    await queryClient.invalidateQueries({ queryKey: ['channel-model-orders'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

const savingModel = computed(() =>
  saveMutation.isPending.value ? (saveMutation.variables.value?.model ?? null) : null,
);

function saveOrder(model: string) {
  const order = findOrder(model);
  if (!order || !isDirty(model)) return;
  saveMutation.mutate({ model, channelIds: [...order.channel_ids] });
}

function refetchAll() {
  void ordersQuery.refetch();
  void channelsQuery.refetch();
}
</script>

<template>
  <!-- eslint-disable vuejs-accessibility/no-static-element-interactions -->
  <div class="flex flex-col">
    <InlineError
      v-if="ordersQuery.isError.value && !ordersQuery.data.value"
      :message="extractApiError(ordersQuery.error.value).message"
      @retry="refetchAll"
    />
    <div v-else class="flex flex-col">
      <DataTable class="[&_[data-slot=table]]:table-fixed" :busy="showTableSkeleton">
        <TableHeader>
          <TableRow>
            <TableHead class="min-w-44">{{ t('pricing.model') }}</TableHead>
            <TableHead>{{ t('models.orderChannels') }}</TableHead>
            <TableHead v-if="canEditOrder" align="center" class="w-28">
              {{ t('common.actions') }}
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" :columns="3" />
          <template v-else>
            <TableRow
              v-for="order in rows"
              :key="order.model"
              data-testid="order-row"
              :data-model="order.model"
            >
              <TableCell class="min-w-0 font-mono font-medium">
                <CopyableName :text="order.model" test-id="order-model-name" />
              </TableCell>
              <TableCell class="min-w-0 whitespace-normal">
                <ol
                  class="m-0 list-none space-y-1 p-0"
                  data-testid="order-channel-list"
                  :data-model="order.model"
                >
                  <li
                    v-for="(channel, index) in order.channels"
                    :key="channel.id"
                    class="flex min-w-0 items-center gap-1.5 rounded border border-transparent px-1 py-0.5"
                    :class="channelDropClass(order.model, index)"
                    data-testid="order-channel"
                    :data-channel="channel.name"
                    :role="canEditOrder ? 'button' : undefined"
                    :tabindex="canEditOrder ? 0 : undefined"
                    @dragover.prevent="onChannelDragOver(order.model, index, $event)"
                    @drop.prevent="onChannelDrop(order.model)"
                  >
                    <button
                      v-if="canEditOrder"
                      type="button"
                      class="text-fg-muted cursor-grab"
                      draggable="true"
                      :aria-label="t('models.orderDragHandle')"
                      :title="t('models.orderDragHandle')"
                      data-testid="order-drag-handle"
                      @dragstart="onChannelDragStart(order.model, index, $event)"
                      @dragend="onDragEnd"
                    >
                      <UiIcon name="grip-vertical" :size="14" />
                    </button>
                    <span
                      class="route-index"
                      :aria-label="t('models.routeHopIndex', { n: index + 1 })"
                    >
                      {{ index + 1 }}
                    </span>
                    <span class="min-w-0 truncate font-mono text-sm">{{ channel.name }}</span>
                    <span
                      v-if="channel.enabled === false"
                      class="badge badge-danger"
                      data-testid="order-channel-disabled"
                    >
                      {{ t('channel.statusDisabled') }}
                    </span>
                  </li>
                </ol>
              </TableCell>
              <TableCell v-if="canEditOrder" align="center">
                <button
                  type="button"
                  class="btn btn-sm"
                  data-testid="order-save"
                  :disabled="!isDirty(order.model) || savingModel === order.model"
                  @click="saveOrder(order.model)"
                >
                  {{ savingModel === order.model ? t('common.saving') : t('models.orderSave') }}
                </button>
              </TableCell>
            </TableRow>
            <TableRow v-if="rows.length === 0">
              <TableCell :colspan="canEditOrder ? 3 : 2" class="h-24 whitespace-normal">
                <EmptyState :title="t('models.orderEmpty')" />
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
    </div>
  </div>
</template>

<style scoped>
.order-drop-before {
  border-color: var(--seed-primary);
  box-shadow: inset 0 2px 0 0 var(--seed-primary);
}
.order-drop-after {
  border-color: var(--seed-primary);
  box-shadow: inset 0 -2px 0 0 var(--seed-primary);
}
</style>
