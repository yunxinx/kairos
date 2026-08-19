<script setup lang="ts">
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import { unifiedMemberWriteBody } from '@/api/types';
import type { ChannelView, Price, UnifiedMember, UnifiedModel } from '@/api/types';
import Checkbox from '@/components/ui/Checkbox.vue';
import DataTablePanel from '@/components/ui/DataTablePanel.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FacetedFilter from '@/components/ui/FacetedFilter.vue';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormSwitch from '@/components/ui/FormSwitch.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import Tooltip from '@/components/ui/Tooltip.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import SplitTable from '@/components/ui/table/SplitTable.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import VirtualTable from '@/components/ui/table/VirtualTable.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { useToast } from '@/composables/useToast';
import ChannelSourceMark from '@/features/models/ChannelSourceMark.vue';
import type { FieldValidationSpec } from '@/lib/form-validation';
import { buildInventory, inventoryRowKey, type InventoryRow } from '@/lib/inventory';
import { moveItem } from '@/lib/move-item';
import {
  channelNameForMember,
  memberSourceKind,
  unifiedMemberKey,
  type MemberSourceKind,
} from '@/lib/unified-sources';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const props = withDefaults(
  defineProps<{
    initial: UnifiedModel | null;
    channels: ChannelView[];
    prices: Price[];
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
const { error } = useToast();
const uid = useId();
const idInputId = `unified-editor-id-${uid}`;
const hideInputId = `unified-editor-hide-${uid}`;

const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();

const initialId = props.initial?.id ?? '';
const initialMembers = props.initial?.models ?? [];
const initialHide = props.initial?.hide ?? false;

const editorId = ref(initialId);
const editorMembers = ref([...initialMembers]);
const editorHide = ref(initialHide);
const dragFrom = ref<number | null>(null);
const dropInsert = ref<number | null>(null);
const searchText = ref('');
const selectedChannels = ref<string[]>([]);

const dirty = computed(
  () =>
    editorId.value !== initialId ||
    editorHide.value !== initialHide ||
    editorMembers.value.map(unifiedMemberKey).join('\0') !==
      initialMembers.map(unifiedMemberKey).join('\0'),
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const inventory = computed(() => buildInventory(props.channels, props.prices));

const channelOptions = computed(() =>
  [...new Set(inventory.value.map((row) => row.channelName))]
    .sort((left, right) => left.localeCompare(right))
    .map((name) => ({ value: name, label: name })),
);

/** 按渠道分行：同一名字在不同渠道上各占一行，勾选互不影响。 */
const pickerRows = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  const channels = new Set(selectedChannels.value);
  return inventory.value.filter((row) => {
    if (channels.size > 0 && !channels.has(row.channelName)) return false;
    if (!q) return true;
    return row.name.toLowerCase().includes(q) || row.channelName.toLowerCase().includes(q);
  });
});

const saveMutation = useMutation({
  mutationFn: (body: UnifiedModel) =>
    props.initial === null
      ? apiClient.createUnifiedModel(body)
      : apiClient.updateUnifiedModel(props.initial.id, body),
  onSuccess: async () => {
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['unified-models'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

function handleSave() {
  const specs: FieldValidationSpec[] = [
    { name: 'id', value: editorId.value, rules: [{ kind: 'required' }] },
  ];
  if (!validate(specs, t)) return;
  saveMutation.mutate({
    id: editorId.value.trim(),
    models: editorMembers.value.map(unifiedMemberWriteBody),
    hide: editorHide.value,
  });
}

function isMember(row: InventoryRow): boolean {
  return editorMembers.value.some(
    (member) => member.channel_id === row.channelId && member.model === row.name,
  );
}

function toggleMember(row: InventoryRow, checked: boolean) {
  if (checked) {
    if (isMember(row)) return;
    editorMembers.value = [...editorMembers.value, { channel_id: row.channelId, model: row.name }];
    return;
  }
  editorMembers.value = editorMembers.value.filter(
    (member) => !(member.channel_id === row.channelId && member.model === row.name),
  );
}

function removeMember(member: UnifiedMember) {
  editorMembers.value = editorMembers.value.filter(
    (item) => unifiedMemberKey(item) !== unifiedMemberKey(member),
  );
}

function moveMember(from: number, to: number) {
  editorMembers.value = moveItem(editorMembers.value, from, to);
}

function onHandleDragStart(index: number, event: DragEvent) {
  dragFrom.value = index;
  event.dataTransfer?.setData('text/plain', String(index));
  if (event.dataTransfer) event.dataTransfer.effectAllowed = 'move';
}

function onDragOver(index: number, event: DragEvent) {
  event.preventDefault();
  if (event.dataTransfer) event.dataTransfer.dropEffect = 'move';
  const el = event.currentTarget as HTMLElement;
  const rect = el.getBoundingClientRect();
  dropInsert.value = event.clientY < rect.top + rect.height / 2 ? index : index + 1;
}

function onDrop() {
  if (dragFrom.value === null || dropInsert.value === null) {
    dragFrom.value = null;
    dropInsert.value = null;
    return;
  }
  const from = dragFrom.value;
  let insert = dropInsert.value;
  if (from < insert) insert -= 1;
  moveMember(from, insert);
  dragFrom.value = null;
  dropInsert.value = null;
}

function onDragEnd() {
  dragFrom.value = null;
  dropInsert.value = null;
}

function rowDropClass(index: number): string {
  if (dropInsert.value === index) return 'drop-insert-before';
  if (dropInsert.value === index + 1 && index === editorMembers.value.length - 1) {
    return 'drop-insert-after';
  }
  return '';
}

function pinnedChannelName(member: UnifiedMember): string {
  return channelNameForMember(props.channels, member);
}

function pickSourceKind(row: InventoryRow): MemberSourceKind {
  const channel = props.channels.find((item) => item.id === row.channelId);
  if (channel === undefined) return 'gone';
  if (!channel.enabled) return 'disabled';
  return 'ok';
}

/** 序号/操作固定；模型名与来源用百分比，避免 `auto` + truncate 把列挤没。 */
const memberColumns = [
  { width: '3.5rem' },
  { width: '40%' },
  { width: '36%' },
  { width: '6.5rem' },
];

/** 勾选固定；模型名与渠道对分剩余。 */
const pickColumns = [{ width: '2.5rem' }, { width: '40%' }, { width: '60%' }];
</script>

<template>
  <FloatingWindow
    :title="initial === null ? t('models.unifiedCreate') : t('models.unifiedEdit')"
    wide
    :anchor="anchor"
    :stack-order="stackOrder"
    :cascade="cascade"
    :attention="attention"
    :topmost="topmost"
    @close="emit('close')"
    @pointerdown="emit('raise')"
  >
    <form novalidate @submit.prevent="handleSave">
      <div class="card-body space-y-3">
        <FormField
          field-name="id"
          :label="t('models.unifiedId')"
          :input-id="idInputId"
          :error="fieldError('id')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="idInputId"
              v-model="editorId"
              type="text"
              :placeholder="t('models.unifiedIdPlaceholder')"
              :disabled="initial !== null"
              data-testid="unified-editor-id"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('id')"
            />
          </template>
        </FormField>

        <div>
          <p class="form-field-label mb-2">{{ t('models.unifiedMembers') }}</p>
          <DataTablePanel class="bounded-table-3">
            <SplitTable class="h-full" :columns="memberColumns" data-testid="unified-member-list">
              <template #header>
                <TableRow>
                  <TableHead class="w-10">{{ t('models.memberIndex') }}</TableHead>
                  <TableHead>{{ t('pricing.model') }}</TableHead>
                  <TableHead>{{ t('models.unifiedSources') }}</TableHead>
                  <TableHead align="center">{{ t('common.actions') }}</TableHead>
                </TableRow>
              </template>
              <TableRow
                v-for="(member, index) in editorMembers"
                :key="unifiedMemberKey(member)"
                :class="rowDropClass(index)"
                data-testid="unified-member"
                :data-member="member.model"
                :data-channel="pinnedChannelName(member)"
                @dragover.prevent="onDragOver(index, $event)"
                @drop.prevent="onDrop"
              >
                <TableCell class="text-fg-muted font-mono text-xs">
                  <span class="inline-flex items-center gap-1">
                    <button
                      type="button"
                      class="text-fg-muted cursor-grab"
                      draggable="true"
                      :aria-label="t('models.unifiedDragHandle')"
                      @dragstart="onHandleDragStart(index, $event)"
                      @dragend="onDragEnd"
                    >
                      <UiIcon name="grip-vertical" :size="14" />
                    </button>
                    {{ index + 1 }}
                  </span>
                </TableCell>
                <TableCell truncate class="font-mono text-sm select-text" :title="member.model">{{
                  member.model
                }}</TableCell>
                <TableCell>
                  <ChannelSourceMark
                    :channel-name="pinnedChannelName(member)"
                    :kind="memberSourceKind(member, channels)"
                  />
                </TableCell>
                <TableCell align="center">
                  <span class="inline-flex items-center justify-center gap-0.5">
                    <button
                      type="button"
                      class="btn btn-ghost btn-icon"
                      data-testid="unified-member-up"
                      :disabled="index === 0"
                      :aria-label="t('models.unifiedMoveUp')"
                      @click="moveMember(index, index - 1)"
                    >
                      <UiIcon name="chevron-up" :size="14" />
                    </button>
                    <button
                      type="button"
                      class="btn btn-ghost btn-icon"
                      data-testid="unified-member-down"
                      :disabled="index === editorMembers.length - 1"
                      :aria-label="t('models.unifiedMoveDown')"
                      @click="moveMember(index, index + 1)"
                    >
                      <UiIcon name="chevron-down" :size="14" />
                    </button>
                    <button
                      type="button"
                      class="btn btn-ghost btn-icon"
                      data-testid="unified-member-remove"
                      :aria-label="t('models.unifiedRemoveMember', { name: member.model })"
                      @click="removeMember(member)"
                    >
                      <UiIcon name="close" :size="14" />
                    </button>
                  </span>
                </TableCell>
              </TableRow>
              <TableRow v-if="editorMembers.length === 0">
                <TableCell :colspan="4" class="h-20 whitespace-normal">
                  <EmptyState :title="t('models.unifiedMembersEmpty')" />
                </TableCell>
              </TableRow>
            </SplitTable>
          </DataTablePanel>
        </div>

        <div>
          <p class="form-field-label mb-2">{{ t('models.unifiedPick') }}</p>
          <div class="mb-2 flex flex-wrap items-center gap-2">
            <SearchInput
              :id="`unified-pick-search-${uid}`"
              v-model="searchText"
              class="max-w-sm"
              data-testid="unified-pick-search"
              :placeholder="t('models.search')"
              :aria-label="t('models.search')"
            />
            <FacetedFilter
              v-model="selectedChannels"
              :title="t('models.channels')"
              :options="channelOptions"
              test-id="unified-pick-channel-filter"
            />
          </div>
          <DataTablePanel class="h-56" data-testid="unified-pick-list">
            <VirtualTable
              class="h-full"
              :rows="pickerRows"
              :colspan="3"
              :columns="pickColumns"
              :get-row-key="(row) => inventoryRowKey(row)"
              :empty-title="t('models.unifiedPickEmpty')"
            >
              <template #header>
                <TableRow>
                  <TableHead class="w-10" />
                  <TableHead>{{ t('pricing.model') }}</TableHead>
                  <TableHead>{{ t('models.channels') }}</TableHead>
                </TableRow>
              </template>
              <template #row="{ row }">
                <TableRow
                  data-testid="unified-pick"
                  :data-model="row.name"
                  :data-channel="row.channelName"
                >
                  <TableCell>
                    <Checkbox
                      :model-value="isMember(row)"
                      data-testid="unified-pick-check"
                      @update:model-value="(value) => toggleMember(row, value)"
                    />
                  </TableCell>
                  <TableCell truncate class="font-mono text-sm" :title="row.name">{{
                    row.name
                  }}</TableCell>
                  <TableCell>
                    <span class="inline-flex max-w-full items-center gap-1">
                      <Tooltip :text="row.channelName">
                        <span class="truncate">{{ row.channelName }}</span>
                      </Tooltip>
                      <ChannelSourceMark
                        v-if="pickSourceKind(row) !== 'ok'"
                        :kind="pickSourceKind(row)"
                      />
                    </span>
                  </TableCell>
                </TableRow>
              </template>
            </VirtualTable>
          </DataTablePanel>
        </div>

        <FormField
          field-name="hide"
          layout="inline"
          :label="t('models.unifiedHide')"
          :input-id="hideInputId"
          :guide="t('models.unifiedHideGuide')"
        >
          <FormSwitch :id="hideInputId" v-model="editorHide" data-testid="unified-hide-switch" />
        </FormField>
      </div>
      <div class="card-footer card-body flex justify-between gap-2">
        <button type="button" class="btn" @click="emit('close')">
          {{ t('common.cancel') }}
        </button>
        <button
          type="submit"
          class="btn btn-primary"
          data-testid="unified-save"
          :disabled="saveMutation.isPending.value"
        >
          {{ t('common.save') }}
        </button>
      </div>
    </form>
  </FloatingWindow>
</template>
