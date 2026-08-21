<script setup lang="ts">
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import { roleAtLeast, type ManagementRole } from '@/api/types';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormPasswordInput from '@/components/ui/FormPasswordInput.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { useToast } from '@/composables/useToast';
import { useCurrentUser } from '@/lib/session';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

withDefaults(
  defineProps<{
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
const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();
const me = useCurrentUser();

const uid = useId();
const emailId = `user-editor-email-${uid}`;
const nameId = `user-editor-name-${uid}`;
const passwordId = `user-editor-password-${uid}`;
const roleId = `user-editor-role-${uid}`;

const email = ref('');
const displayName = ref('');
const password = ref('');
const role = ref<ManagementRole>('user');

const canPickRole = computed(() => {
  const current = me.value?.role;
  return current !== undefined && roleAtLeast(current, 'root');
});
const roleOptions = computed(() => [
  { value: 'user', label: t('users.roleUser') },
  { value: 'admin', label: t('users.roleAdmin') },
]);

const dirty = computed(
  () =>
    email.value.trim() !== '' ||
    displayName.value.trim() !== '' ||
    password.value !== '' ||
    role.value !== 'user',
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const saveMutation = useMutation({
  mutationFn: () =>
    apiClient.createUser({
      email: email.value.trim(),
      display_name: displayName.value.trim(),
      password: password.value,
      role: canPickRole.value ? role.value : 'user',
    }),
  onSuccess: async () => {
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['users'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

function handleSave() {
  if (
    !validate(
      [
        { name: 'email', value: email.value, rules: [{ kind: 'required' }] },
        { name: 'displayName', value: displayName.value, rules: [{ kind: 'required' }] },
        {
          name: 'password',
          value: password.value,
          rules: [{ kind: 'required' }, { kind: 'minLength', min: 8 }],
        },
      ],
      t,
    )
  ) {
    return;
  }
  saveMutation.mutate();
}
</script>

<template>
  <FloatingWindow
    :title="t('users.editorCreate')"
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
          field-name="email"
          :label="t('users.email')"
          :input-id="emailId"
          :error="fieldError('email')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="emailId"
              v-model="email"
              type="email"
              autocomplete="off"
              data-testid="user-editor-email"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('email')"
            />
          </template>
        </FormField>
        <FormField
          field-name="displayName"
          :label="t('users.displayName')"
          :input-id="nameId"
          :error="fieldError('displayName')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="nameId"
              v-model="displayName"
              type="text"
              data-testid="user-editor-display-name"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('displayName')"
            />
          </template>
        </FormField>
        <FormField
          field-name="password"
          :label="t('users.password')"
          :input-id="passwordId"
          :error="fieldError('password')"
          :guide="t('users.passwordGuide')"
        >
          <template #default="{ hintId, invalid }">
            <FormPasswordInput
              :id="passwordId"
              v-model="password"
              autocomplete="new-password"
              data-testid="user-editor-password"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('password')"
            />
          </template>
        </FormField>
        <FormField v-if="canPickRole" field-name="role" :label="t('users.role')" :input-id="roleId">
          <UiSelect
            :id="roleId"
            v-model="role"
            :options="roleOptions"
            data-testid="user-editor-role"
          />
        </FormField>
      </div>
      <div class="card-footer card-body flex justify-end gap-2">
        <button type="button" class="btn" @click="emit('close')">{{ t('common.cancel') }}</button>
        <button
          type="submit"
          class="btn btn-primary"
          data-testid="user-save"
          :disabled="saveMutation.isPending.value"
        >
          {{ t('common.create') }}
        </button>
      </div>
    </form>
  </FloatingWindow>
</template>
