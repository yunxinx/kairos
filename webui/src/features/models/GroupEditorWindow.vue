<script setup lang="ts">
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { ModelGroup } from '@/api/types';
import Checkbox from '@/components/ui/Checkbox.vue';
import DataTablePanel from '@/components/ui/DataTablePanel.vue';
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
import { DEFAULT_MODEL_GROUP } from '@/lib/visible-models';
import type { FieldValidationSpec } from '@/lib/form-validation';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const props = withDefaults(
  defineProps<{
    initial: ModelGroup | null;
    callableNames: string[];
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
const initialModels = props.initial?.models ?? [];

const editorName = ref(initialName);
const editorModels = ref([...initialModels]);
const searchText = ref('');

/** 组内表：模型名用百分比，避免 `auto` + truncate 把列挤没。 */
const memberColumns = [{ width: '85%' }, { width: '3.5rem' }];
/** 可用表：勾选固定，模型名用百分比。 */
const pickColumns = [{ width: '2.5rem' }, { width: '90%' }];

const dirty = computed(
  () =>
    editorName.value !== initialName ||
    [...editorModels.value].sort().join('\0') !== [...initialModels].sort().join('\0'),
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const pickerRows = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  return props.callableNames
    .filter((name) => !editorModels.value.includes(name))
    .filter((name) => !q || name.toLowerCase().includes(q))
    .sort((left, right) => left.localeCompare(right));
});

function addModel(name: string) {
  if (!name || editorModels.value.includes(name)) return;
  editorModels.value = [...editorModels.value, name];
}

function onPickCheck(name: string, checked: boolean) {
  if (checked) addModel(name);
}

function removeModel(name: string) {
  editorModels.value = editorModels.value.filter((item) => item !== name);
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
    models: [...editorModels.value],
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
              :rows="editorModels"
              :colspan="2"
              :columns="memberColumns"
              data-testid="group-model-list"
              :get-row-key="(name) => name"
              :empty-title="t('models.groupModelsEmpty')"
            >
              <template #header>
                <TableRow>
                  <TableHead>{{ t('pricing.model') }}</TableHead>
                  <TableHead align="center">{{ t('common.actions') }}</TableHead>
                </TableRow>
              </template>
              <template #row="{ row: name }">
                <TableRow data-testid="group-model-option" :data-model="name">
                  <TableCell truncate class="font-mono text-sm" :title="name">{{ name }}</TableCell>
                  <TableCell align="center">
                    <button
                      type="button"
                      class="btn btn-ghost btn-icon"
                      data-testid="group-model-remove"
                      :aria-label="t('models.groupRemoveModel', { name })"
                      @click="removeModel(name)"
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
          <SearchInput
            :id="`group-editor-search-${uid}`"
            v-model="searchText"
            class="mb-2 max-w-sm"
            data-testid="group-editor-search"
            :placeholder="t('models.search')"
            :aria-label="t('models.search')"
          />
          <DataTablePanel class="h-56" data-testid="group-pick-list">
            <VirtualTable
              class="h-full"
              :rows="pickerRows"
              :colspan="2"
              :columns="pickColumns"
              :get-row-key="(name) => name"
              :empty-title="t('models.groupPickEmpty')"
            >
              <template #header>
                <TableRow>
                  <TableHead class="w-10" />
                  <TableHead>{{ t('pricing.model') }}</TableHead>
                </TableRow>
              </template>
              <template #row="{ row: name }">
                <TableRow data-testid="group-pick" :data-model="name">
                  <TableCell>
                    <Checkbox
                      :model-value="false"
                      data-testid="group-pick-check"
                      @update:model-value="(value) => onPickCheck(name, value)"
                    />
                  </TableCell>
                  <TableCell truncate class="font-mono text-sm" :title="name">{{ name }}</TableCell>
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
