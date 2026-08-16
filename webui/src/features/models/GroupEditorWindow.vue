<script setup lang="ts">
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { ModelGroup } from '@/api/types';
import DataTablePanel from '@/components/ui/DataTablePanel.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import Table from '@/components/ui/table/Table.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import { useFormValidation } from '@/composables/useFormValidation';
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
const uid = useId();
const nameInputId = `group-editor-name-${uid}`;
const addInputId = `group-editor-add-${uid}`;

const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();

const isDefault = computed(
  () => props.initial !== null && props.initial.name === DEFAULT_MODEL_GROUP,
);

const initialName = props.initial?.name ?? '';
const initialModels = props.initial?.models ?? [];

const editorName = ref(initialName);
const editorModels = ref([...initialModels]);
const editorError = ref('');
const searchText = ref('');
const editorAdd = ref('');

const dirty = computed(
  () =>
    editorName.value !== initialName ||
    [...editorModels.value].sort().join('\0') !== [...initialModels].sort().join('\0'),
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const filteredMembers = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  const names = [...editorModels.value];
  if (!q) return names;
  return names.filter((name) => name.toLowerCase().includes(q));
});

const addOptions = computed(() =>
  props.callableNames
    .filter((name) => !editorModels.value.includes(name))
    .sort((left, right) => left.localeCompare(right))
    .map((name) => ({ value: name, label: name })),
);

const addSelection = computed({
  get: () => {
    const options = addOptions.value;
    if (options.some((item) => item.value === editorAdd.value)) return editorAdd.value;
    return options[0]?.value ?? '';
  },
  set: (value: string) => {
    editorAdd.value = value;
  },
});

function addModel() {
  const name = addSelection.value.trim();
  if (!name || editorModels.value.includes(name)) return;
  editorModels.value = [...editorModels.value, name];
  editorAdd.value = '';
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
    editorError.value = '';
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['model-groups'] });
  },
  onError: (err) => {
    editorError.value = extractApiError(err).message;
  },
});

function handleSave() {
  editorError.value = '';
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
          <p class="form-field-label mb-2">{{ t('models.groupModels') }}</p>
          <SearchInput
            :id="`group-editor-search-${uid}`"
            v-model="searchText"
            class="mb-2 max-w-sm"
            data-testid="group-editor-search"
            :placeholder="t('models.groupSearch')"
            :aria-label="t('models.groupSearch')"
          />
          <DataTablePanel>
            <div class="seed-scrollbar max-h-56 overflow-y-auto">
              <Table data-testid="group-model-list">
                <TableHeader>
                  <TableRow>
                    <TableHead>{{ t('pricing.model') }}</TableHead>
                    <TableHead align="center">{{ t('common.actions') }}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  <TableRow
                    v-for="name in filteredMembers"
                    :key="name"
                    data-testid="group-model-option"
                    :data-model="name"
                  >
                    <TableCell class="font-mono text-sm">{{ name }}</TableCell>
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
                  <TableRow v-if="filteredMembers.length === 0">
                    <TableCell :colspan="2" class="h-20 whitespace-normal">
                      <EmptyState :title="t('models.groupModelsEmpty')" />
                    </TableCell>
                  </TableRow>
                </TableBody>
              </Table>
            </div>
          </DataTablePanel>
          <div v-if="addOptions.length > 0" class="mt-2 flex items-end gap-2">
            <FormField
              class="min-w-0 flex-1"
              field-name="addModel"
              :label="t('models.groupAddModel')"
              :input-id="addInputId"
            >
              <UiSelect
                :id="addInputId"
                v-model="addSelection"
                :options="addOptions"
                data-testid="group-add-select"
              />
            </FormField>
            <button
              type="button"
              class="btn mb-0.5"
              data-testid="group-add-member"
              @click="addModel"
            >
              {{ t('models.groupAdd') }}
            </button>
          </div>
        </div>

        <p v-if="editorError" class="text-danger text-sm" data-testid="group-editor-error">
          {{ editorError }}
        </p>
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
