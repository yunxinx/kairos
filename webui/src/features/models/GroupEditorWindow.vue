<script setup lang="ts">
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { ModelGroup } from '@/api/types';
import Checkbox from '@/components/ui/Checkbox.vue';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
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

const dirty = computed(
  () =>
    editorName.value !== initialName ||
    [...editorModels.value].sort().join('\0') !== [...initialModels].sort().join('\0'),
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const filteredNames = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  const names = [...props.callableNames].sort((left, right) => left.localeCompare(right));
  if (!q) return names;
  return names.filter((name) => name.toLowerCase().includes(q));
});

function isChecked(name: string): boolean {
  return editorModels.value.includes(name);
}

function toggleModel(name: string) {
  if (editorModels.value.includes(name)) {
    editorModels.value = editorModels.value.filter((item) => item !== name);
  } else {
    editorModels.value = [...editorModels.value, name];
  }
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
          <ul class="max-h-64 space-y-1 overflow-y-auto" data-testid="group-model-list">
            <li
              v-for="name in filteredNames"
              :key="name"
              class="flex items-center gap-2 py-0.5"
              data-testid="group-model-option"
              :data-model="name"
            >
              <Checkbox
                :model-value="isChecked(name)"
                :data-testid="'group-model-check'"
                :aria-label="name"
                @update:model-value="() => toggleModel(name)"
              />
              <span class="font-mono text-sm">{{ name }}</span>
            </li>
          </ul>
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
