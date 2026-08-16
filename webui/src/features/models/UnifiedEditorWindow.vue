<script setup lang="ts">
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { UnifiedModel } from '@/api/types';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormSwitch from '@/components/ui/FormSwitch.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import type { FieldValidationSpec } from '@/lib/form-validation';
import { moveItem } from '@/lib/move-item';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const props = withDefaults(
  defineProps<{
    initial: UnifiedModel | null;
    memberOptions: string[];
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
const idInputId = `unified-editor-id-${uid}`;
const hideInputId = `unified-editor-hide-${uid}`;
const addInputId = `unified-editor-add-${uid}`;

const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();

const initialId = props.initial?.id ?? '';
const initialMembers = props.initial?.models ?? [];
const initialHide = props.initial?.hide ?? false;

const editorId = ref(initialId);
const editorMembers = ref([...initialMembers]);
const editorHide = ref(initialHide);
const editorAdd = ref('');
const editorError = ref('');
const dragFrom = ref<number | null>(null);

const dirty = computed(
  () =>
    editorId.value !== initialId ||
    editorHide.value !== initialHide ||
    editorMembers.value.join('\0') !== initialMembers.join('\0'),
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const addOptions = computed(() =>
  props.memberOptions
    .filter((name) => !editorMembers.value.includes(name))
    .map((name) => ({ value: name, label: name })),
);

/** 展示值从剩余选项派生；用户选择写入 `editorAdd`。 */
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

const saveMutation = useMutation({
  mutationFn: (body: UnifiedModel) =>
    props.initial === null
      ? apiClient.createUnifiedModel(body)
      : apiClient.updateUnifiedModel(props.initial.id, body),
  onSuccess: async () => {
    editorError.value = '';
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['unified-models'] });
  },
  onError: (err) => {
    editorError.value = extractApiError(err).message;
  },
});

function handleSave() {
  editorError.value = '';
  const specs: FieldValidationSpec[] = [
    { name: 'id', value: editorId.value, rules: [{ kind: 'required' }] },
  ];
  if (!validate(specs, t)) return;
  saveMutation.mutate({
    id: editorId.value.trim(),
    models: [...editorMembers.value],
    hide: editorHide.value,
  });
}

function addMember() {
  const name = addSelection.value.trim();
  if (!name || editorMembers.value.includes(name)) return;
  editorMembers.value = [...editorMembers.value, name];
  editorAdd.value = '';
}

function removeMember(name: string) {
  editorMembers.value = editorMembers.value.filter((item) => item !== name);
}

function moveMember(from: number, to: number) {
  editorMembers.value = moveItem(editorMembers.value, from, to);
}

function onDragStart(index: number, event: DragEvent) {
  dragFrom.value = index;
  event.dataTransfer?.setData('text/plain', String(index));
  if (event.dataTransfer) event.dataTransfer.effectAllowed = 'move';
}

function onDrop(index: number) {
  if (dragFrom.value === null) return;
  moveMember(dragFrom.value, index);
  dragFrom.value = null;
}
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
          <ol class="space-y-1" data-testid="unified-member-list">
            <!-- HTML5 拖拽落点在行上；键盘重排由上/下按钮承担。 -->
            <!-- eslint-disable vuejs-accessibility/no-static-element-interactions -->
            <li
              v-for="(member, index) in editorMembers"
              :key="member"
              class="border-seed flex items-center gap-2 rounded-md border px-2 py-1"
              draggable="true"
              data-testid="unified-member"
              :data-member="member"
              @dragstart="onDragStart(index, $event)"
              @dragover.prevent
              @drop.prevent="onDrop(index)"
            >
              <button
                type="button"
                class="text-fg-muted cursor-grab"
                :aria-label="t('models.unifiedDragHandle')"
              >
                <UiIcon name="grip-vertical" :size="14" />
              </button>
              <span class="min-w-0 flex-1 font-mono text-sm">{{ member }}</span>
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
                :aria-label="t('models.unifiedRemoveMember', { name: member })"
                @click="removeMember(member)"
              >
                <UiIcon name="close" :size="14" />
              </button>
            </li>
            <!-- eslint-enable vuejs-accessibility/no-static-element-interactions -->
          </ol>
          <div v-if="addOptions.length > 0" class="mt-2 flex items-end gap-2">
            <FormField
              class="min-w-0 flex-1"
              field-name="addMember"
              :label="t('models.unifiedAddMember')"
              :input-id="addInputId"
            >
              <UiSelect
                :id="addInputId"
                v-model="addSelection"
                :options="addOptions"
                data-testid="unified-add-select"
              />
            </FormField>
            <button
              type="button"
              class="btn mb-0.5"
              data-testid="unified-add-member"
              @click="addMember"
            >
              {{ t('models.unifiedAdd') }}
            </button>
          </div>
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

        <p v-if="editorError" class="text-danger text-sm" data-testid="unified-editor-error">
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
          data-testid="unified-save"
          :disabled="saveMutation.isPending.value"
        >
          {{ t('common.save') }}
        </button>
      </div>
    </form>
  </FloatingWindow>
</template>
