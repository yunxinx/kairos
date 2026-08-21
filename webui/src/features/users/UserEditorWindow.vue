<script setup lang="ts">
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import { roleAtLeast, type ManagementRole, type UserAdminView } from '@/api/types';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import FormField from '@/components/ui/FormField.vue';
import FormPasswordInput from '@/components/ui/FormPasswordInput.vue';
import FormSwitch from '@/components/ui/FormSwitch.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { useToast } from '@/composables/useToast';
import { useCurrentUser } from '@/lib/session';
import type { FieldValidationSpec } from '@/lib/form-validation';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const props = withDefaults(
  defineProps<{
    initial?: UserAdminView | null;
    anchor?: FloatingWindowAnchor | null;
    stackOrder?: number;
    cascade?: number;
    attention?: boolean;
    topmost?: boolean;
  }>(),
  { initial: null, anchor: null, stackOrder: 0, cascade: 0, attention: false, topmost: true },
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
const rpmId = `user-editor-rpm-${uid}`;
const enabledId = `user-editor-enabled-${uid}`;

const initialEmail = props.initial ? props.initial.email : '';
const initialName = props.initial ? props.initial.display_name : '';
const initialRole = props.initial ? props.initial.role : 'user';
const initialEnabled = props.initial ? props.initial.enabled : true;
const initialRpm =
  props.initial && props.initial.rate_limit_rpm !== null && props.initial.rate_limit_rpm !== undefined
    ? String(props.initial.rate_limit_rpm)
    : '';

const email = ref(initialEmail);
const displayName = ref(initialName);
const password = ref('');
const role = ref<ManagementRole>(initialRole);
const rateLimitRpm = ref(initialRpm);
const enabled = ref(initialEnabled);

const isCreate = computed(() => props.initial === null);

const windowTitle = computed(() => {
  if (isCreate.value) {
    return t('users.editorCreate');
  }
  const name = props.initial?.display_name || props.initial?.email || '';
  const mail = props.initial?.email || '';
  return name && name !== mail ? `${name} (${mail})` : mail;
});

const canPickRole = computed(() => {
  const current = me.value?.role;
  return current !== undefined && roleAtLeast(current, 'root');
});

const roleOptions = computed(() => [
  { value: 'user', label: t('users.roleUser') },
  { value: 'admin', label: t('users.roleAdmin') },
]);

const dirty = computed(() => {
  if (isCreate.value) {
    return (
      email.value.trim() !== '' ||
      displayName.value.trim() !== '' ||
      password.value !== '' ||
      role.value !== 'user' ||
      rateLimitRpm.value.trim() !== ''
    );
  }
  return (
    displayName.value !== initialName ||
    password.value !== '' ||
    role.value !== initialRole ||
    enabled.value !== initialEnabled ||
    rateLimitRpm.value.trim() !== initialRpm
  );
});
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const saveMutation = useMutation({
  mutationFn: async () => {
    const parsedRpm = rateLimitRpm.value.trim() === '' ? null : Number(rateLimitRpm.value);
    if (isCreate.value) {
      return apiClient.createUser({
        email: email.value.trim(),
        display_name: displayName.value.trim(),
        password: password.value,
        role: canPickRole.value ? role.value : 'user',
        rate_limit_rpm: parsedRpm,
      });
    } else if (props.initial) {
      const body: {
        display_name?: string;
        role?: ManagementRole;
        enabled?: boolean;
        password?: string;
        rate_limit_rpm?: number | null;
      } = {
        display_name: displayName.value.trim(),
        enabled: enabled.value,
        rate_limit_rpm: parsedRpm,
      };
      if (canPickRole.value) {
        body.role = role.value;
      }
      if (password.value) {
        body.password = password.value;
      }
      return apiClient.updateUser(props.initial.id, body);
    }
  },
  onSuccess: async () => {
    emit('close');
    await queryClient.invalidateQueries({ queryKey: ['users'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

function handleSave() {
  const specs: FieldValidationSpec[] = [
    { name: 'displayName', value: displayName.value, rules: [{ kind: 'required' }] },
  ];
  if (isCreate.value) {
    specs.push(
      { name: 'email', value: email.value, rules: [{ kind: 'required' }] },
      {
        name: 'password',
        value: password.value,
        rules: [{ kind: 'required' }, { kind: 'minLength', min: 8 }],
      },
    );
  } else if (password.value) {
    specs.push({
      name: 'password',
      value: password.value,
      rules: [{ kind: 'minLength', min: 8 }],
    });
  }
    if (rateLimitRpm.value.trim()) {
      specs.push({
        name: 'rateLimitRpm',
        value: rateLimitRpm.value,
        rules: [{ kind: 'uint' }],
      });
    }

  if (!validate(specs, t)) return;
  saveMutation.mutate();
}
</script>

<template>
  <FloatingWindow
    :title="windowTitle"
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
              :disabled="!isCreate"
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
          :label="isCreate ? t('users.password') : t('users.resetPassword')"
          :input-id="passwordId"
          :error="fieldError('password')"
          :guide="isCreate ? t('users.passwordGuide') : t('users.resetPasswordGuide')"
        >
          <template #default="{ hintId, invalid }">
            <FormPasswordInput
              :id="passwordId"
              v-model="password"
              autocomplete="new-password"
              data-testid="user-editor-password"
              :placeholder="isCreate ? '' : '••••••••'"
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

        <FormField
          field-name="rateLimitRpm"
          :label="t('users.rateLimitRpm')"
          :input-id="rpmId"
          :error="fieldError('rateLimitRpm')"
          :guide="t('users.rateLimitRpmGuide')"
        >
          <template #default="{ hintId, invalid }">
            <FormTextInput
              :id="rpmId"
              v-model="rateLimitRpm"
              type="text"
              inputmode="numeric"
              class="font-mono"
              placeholder="0"
              data-testid="user-editor-rpm"
              :invalid="invalid"
              :hint-id="hintId"
              v-on="fieldInputHandlers('rateLimitRpm')"
            />
          </template>
        </FormField>

        <FormField
          v-if="!isCreate"
          field-name="enabled"
          layout="inline"
          :label="t('users.enabled')"
          :input-id="enabledId"
        >
          <FormSwitch :id="enabledId" v-model="enabled" data-testid="user-editor-enabled" />
        </FormField>
      </div>
      <div class="card-footer card-body flex justify-between gap-2">
        <button type="button" class="btn" @click="emit('close')">{{ t('common.cancel') }}</button>
        <button
          type="submit"
          class="btn btn-primary"
          data-testid="user-save"
          :disabled="saveMutation.isPending.value"
        >
          {{ t('common.save') }}
        </button>
      </div>
    </form>
  </FloatingWindow>
</template>
