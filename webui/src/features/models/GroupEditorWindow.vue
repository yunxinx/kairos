<script setup lang="ts">
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { ChannelView, GroupModel, ModelGroup, UnifiedModel } from '@/api/types';
import Checkbox from '@/components/ui/Checkbox.vue';
import DataTablePanel from '@/components/ui/DataTablePanel.vue';
import FacetedFilter from '@/components/ui/FacetedFilter.vue';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import VirtualTable from '@/components/ui/table/VirtualTable.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { useToast } from '@/composables/useToast';
import CallableSourceCell from '@/features/models/CallableSourceCell.vue';
import ChannelSourceMark from '@/features/models/ChannelSourceMark.vue';
import UnifiedNameChip from '@/features/models/UnifiedNameChip.vue';
import { countedFacetOptions } from '@/lib/faceted-filter';
import type { FieldValidationSpec } from '@/lib/form-validation';
import {
  groupMemberSourceLine,
  groupModelKey,
  groupPickKey,
  groupPickRows,
  pickRowIsMember,
  pickRowToMember,
  type GroupPickRow,
} from '@/lib/group-models';
import { DEFAULT_MODEL_GROUP } from '@/lib/visible-models';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const props = withDefaults(
  defineProps<{
    initial: ModelGroup | null;
    channels: ChannelView[];
    unifiedModels: UnifiedModel[];
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
const nameInputId = `group-editor-name-${uid}`;

const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();

const isDefault = computed(
  () => props.initial !== null && props.initial.name === DEFAULT_MODEL_GROUP,
);

const initialName = props.initial?.name ?? '';
const initialMembers = props.initial?.models ?? [];
const initialMemberKeys = initialMembers.map(groupModelKey).join('\0');

/** 渠道筛选里把统一模型单独成档，不当成真渠道名。 */
const UNIFIED_SOURCE_FILTER = '__unified__';

const editorName = ref(initialName);
const editorMembers = ref<GroupModel[]>([...initialMembers]);
const searchText = ref('');
const selectedChannels = ref<string[]>([]);

/** 组内表：模型名与来源用百分比，操作列固定。 */
const memberColumns = [{ width: '40%' }, { width: '45%' }, { width: '3.5rem' }];
/** 可用表：勾选固定，模型名与来源对分剩余。 */
const pickColumns = [{ width: '2.5rem' }, { width: '40%' }, { width: '55%' }];

const dirty = computed(
  () =>
    editorName.value !== initialName ||
    editorMembers.value.map(groupModelKey).join('\0') !== initialMemberKeys,
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const memberRows = computed(() =>
  editorMembers.value.map((member) =>
    groupMemberSourceLine(member, props.channels, props.unifiedModels),
  ),
);

const allPickRows = computed(() => groupPickRows(props.channels, props.unifiedModels));

const availableRows = computed(() =>
  allPickRows.value.filter((row) => !pickRowIsMember(row, editorMembers.value)),
);

const channelOptions = computed(() => {
  const options = countedFacetOptions(
    availableRows.value.filter((row) => row.kind === 'source').map((row) => row.channelName),
  );
  const unifiedCount = availableRows.value.filter((row) => row.kind === 'unified').length;
  if (unifiedCount > 0) {
    options.unshift({
      value: UNIFIED_SOURCE_FILTER,
      label: t('models.unifiedChipTooltip'),
      count: unifiedCount,
    });
  }
  return options;
});

const pickerRows = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  const channels = new Set(selectedChannels.value);
  const unifiedLabel = t('models.unifiedChipTooltip').toLowerCase();
  return availableRows.value
    .filter((row) => {
      if (channels.size > 0) {
        if (row.kind === 'unified') {
          if (!channels.has(UNIFIED_SOURCE_FILTER)) return false;
        } else if (!channels.has(row.channelName)) {
          return false;
        }
      }
      if (!q) return true;
      const channelHit = row.kind === 'source' && row.channelName.toLowerCase().includes(q);
      return (
        row.name.toLowerCase().includes(q) ||
        channelHit ||
        (row.kind === 'unified' && unifiedLabel.includes(q))
      );
    })
    .sort((left, right) => {
      const byName = left.name.localeCompare(right.name);
      if (byName !== 0) return byName;
      if (left.kind === 'source' && right.kind === 'source') {
        return left.channelName.localeCompare(right.channelName);
      }
      return 0;
    });
});

function addRow(row: GroupPickRow) {
  if (pickRowIsMember(row, editorMembers.value)) return;
  editorMembers.value = [...editorMembers.value, pickRowToMember(row)];
}

function onPickCheck(row: GroupPickRow, checked: boolean) {
  if (checked) addRow(row);
}

function removeByKey(key: string) {
  editorMembers.value = editorMembers.value.filter((item) => groupModelKey(item) !== key);
}

const saveMutation = useMutation({
  mutationFn: (body: ModelGroup) =>
    props.initial === null
      ? apiClient.createModelGroup(body)
      : apiClient.updateModelGroup(props.initial.name, body),
  onSuccess: async () => {
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['model-groups'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

function handleSave() {
  const specs: FieldValidationSpec[] = [
    { name: 'name', value: editorName.value, rules: [{ kind: 'required' }] },
  ];
  if (!validate(specs, t)) return;
  saveMutation.mutate({
    name: editorName.value.trim(),
    models: [...editorMembers.value],
  });
}
</script>

<template>
  <FloatingWindow
    :title="initial === null ? t('models.groupCreate') : t('models.groupEdit')"
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
          field-name="name"
          :label="t('models.groupName')"
          :input-id="nameInputId"
          :error="fieldError('name')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="nameInputId"
              v-model="editorName"
              type="text"
              :placeholder="t('models.groupNamePlaceholder')"
              :disabled="isDefault"
              data-testid="group-editor-name"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('name')"
            />
          </template>
        </FormField>
        <p v-if="isDefault" class="text-fg-muted text-xs">{{ t('models.groupDefaultHint') }}</p>

        <div>
          <p class="form-field-label mb-2">{{ t('models.groupMembers') }}</p>
          <DataTablePanel class="h-56">
            <VirtualTable
              class="h-full"
              :rows="memberRows"
              :colspan="3"
              :columns="memberColumns"
              data-testid="group-model-list"
              :get-row-key="(row) => row.key"
              :empty-title="t('models.groupModelsEmpty')"
            >
              <template #header>
                <TableRow>
                  <TableHead>{{ t('pricing.model') }}</TableHead>
                  <TableHead>{{ t('models.unifiedSources') }}</TableHead>
                  <TableHead align="center">{{ t('common.actions') }}</TableHead>
                </TableRow>
              </template>
              <template #row="{ row }">
                <TableRow
                  data-testid="group-model-option"
                  :data-model="row.name"
                  :data-channel="row.channels[0]?.name"
                >
                  <TableCell truncate :title="row.name">
                    <UnifiedNameChip v-if="row.isUnified" :name="row.name" />
                    <span v-else class="font-mono text-sm">{{ row.name }}</span>
                  </TableCell>
                  <TableCell>
                    <CallableSourceCell :line="row" />
                  </TableCell>
                  <TableCell align="center">
                    <button
                      type="button"
                      class="btn btn-ghost btn-icon"
                      data-testid="group-model-remove"
                      :aria-label="t('models.groupRemoveModel', { name: row.name })"
                      @click="removeByKey(row.key)"
                    >
                      <UiIcon name="close" :size="14" />
                    </button>
                  </TableCell>
                </TableRow>
              </template>
            </VirtualTable>
          </DataTablePanel>
        </div>

        <div>
          <p class="form-field-label mb-2">{{ t('models.groupModels') }}</p>
          <div class="mb-2 flex flex-wrap items-center gap-2">
            <SearchInput
              :id="`group-editor-search-${uid}`"
              v-model="searchText"
              class="max-w-sm"
              data-testid="group-editor-search"
              :placeholder="t('models.search')"
              :aria-label="t('models.search')"
            />
            <FacetedFilter
              v-model="selectedChannels"
              :title="t('models.channels')"
              :options="channelOptions"
              test-id="group-pick-channel-filter"
            />
          </div>
          <DataTablePanel class="h-56" data-testid="group-pick-list">
            <VirtualTable
              class="h-full"
              :rows="pickerRows"
              :colspan="3"
              :columns="pickColumns"
              :get-row-key="(row) => groupPickKey(row)"
              :empty-title="t('models.groupPickEmpty')"
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
                  data-testid="group-pick"
                  :data-model="row.name"
                  :data-channel="row.kind === 'source' ? row.channelName : undefined"
                >
                  <TableCell>
                    <Checkbox
                      :model-value="false"
                      data-testid="group-pick-check"
                      @update:model-value="(value) => onPickCheck(row, value)"
                    />
                  </TableCell>
                  <TableCell truncate :title="row.name">
                    <UnifiedNameChip v-if="row.kind === 'unified'" :name="row.name" />
                    <span v-else class="font-mono text-sm">{{ row.name }}</span>
                  </TableCell>
                  <TableCell>
                    <ChannelSourceMark
                      v-if="row.kind === 'source'"
                      :channel-name="row.channelName"
                      :kind="row.channelKind"
                      chip-test-id="group-source-channel"
                    />
                    <CallableSourceCell
                      v-else
                      :line="
                        groupMemberSourceLine(
                          { kind: 'unified', id: row.name },
                          channels,
                          unifiedModels,
                        )
                      "
                    />
                  </TableCell>
                </TableRow>
              </template>
            </VirtualTable>
          </DataTablePanel>
        </div>
      </div>
      <div class="card-footer card-body flex justify-between gap-2">
        <button type="button" class="btn" @click="emit('close')">
          {{ t('common.cancel') }}
        </button>
        <button
          type="submit"
          class="btn btn-primary"
          data-testid="group-save"
          :disabled="saveMutation.isPending.value"
        >
          {{ t('common.save') }}
        </button>
      </div>
    </form>
  </FloatingWindow>
</template>
