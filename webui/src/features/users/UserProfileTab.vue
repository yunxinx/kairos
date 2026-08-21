<script setup lang="ts">
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import { roleAtLeast, type ManagementRole, type UserAdminView } from '@/api/types';
import FormField from '@/components/ui/FormField.vue';
import FormPasswordInput from '@/components/ui/FormPasswordInput.vue';
import FormSwitch from '@/components/ui/FormSwitch.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { useToast } from '@/composables/useToast';
import { useCurrentUser } from '@/lib/session';
import type { FieldValidationSpec } from '@/lib/form-validation';

const props = defineProps<{
  user: UserAdminView;
}>();

const emit = defineEmits<{
  close: [];
  'dirty-change': [dirty: boolean];
}>();

const { t } = useI18n();
const { error, success } = useToast();
const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();
const me = useCurrentUser();

const uid = useId();
const nameId = `user-profile-name-${uid}`;
const passwordId = `user-profile-password-${uid}`;
const roleId = `user-profile-role-${uid}`;
const rpmId = `user-profile-rpm-${uid}`;
const enabledId = `user-profile-enabled-${uid}`;

const initialName = props.user.display_name;
const initialRole = props.user.role;
const initialEnabled = props.user.enabled;
const initialRpm =
  props.user.rate_limit_rpm !== null && props.user.rate_limit_rpm !== undefined
    ? String(props.user.rate_limit_rpm)
    : '';

const displayName = ref(initialName);
const password = ref('');
const role = ref<ManagementRole>(initialRole);
const rateLimitRpm = ref(initialRpm);
const enabled = ref(initialEnabled);

const isRootUser = computed(() => props.user.role === 'root');

const canPickRole = computed(() => {
  if (isRootUser.value) return false;
  const current = me.value?.role;
  return current !== undefined && roleAtLeast(current, 'root');
});

const roleOptions = computed(() => {
  if (isRootUser.value) {
    return [{ value: 'root', label: t('users.roleRoot') }];
  }
  return [
    { value: 'user', label: t('users.roleUser') },
    { value: 'admin', label: t('users.roleAdmin') },
  ];
});

const dirty = computed(
  () =>
    displayName.value !== initialName ||
    password.value !== '' ||
    role.value !== initialRole ||
    enabled.value !== initialEnabled ||
    rateLimitRpm.value.trim() !== initialRpm,
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const saveMutation = useMutation({
  mutationFn: async () => {
    const parsedRpm = rateLimitRpm.value.trim() === '' ? null : Number(rateLimitRpm.value);
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
    return apiClient.updateUser(props.user.id, body);
  },
  onSuccess: async () => {
    success(t('users.updateSuccess'));
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
  if (password.value) {
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
  <form novalidate @submit.prevent="handleSave">
    <div class="card-body space-y-3">
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
        :label="t('users.resetPassword')"
        :input-id="passwordId"
        :error="fieldError('password')"
        :guide="t('users.resetPasswordGuide')"
      >
        <template #default="{ hintId, invalid }">
          <FormPasswordInput
            :id="passwordId"
            v-model="password"
            autocomplete="new-password"
            data-testid="user-editor-password"
            placeholder="••••••••"
            :invalid="invalid"
            :hint-id="hintId"
            v-on="fieldInputHandlers('password')"
          />
        </template>
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

      <div
        :class="{
          'border-seed bg-surface-alt/40 rounded-md border border-dashed p-3 opacity-60': isRootUser,
        }"
      >
        <FormField
          field-name="role"
          :label="t('users.role')"
          :input-id="roleId"
          :guide="isRootUser ? t('users.rootProtectedGuide') : undefined"
        >
          <UiSelect
            :id="roleId"
            v-model="role"
            :options="roleOptions"
            :disabled="isRootUser || !canPickRole"
            data-testid="user-editor-role"
          />
        </FormField>
      </div>

      <div
        :class="{
          'border-seed bg-surface-alt/40 rounded-md border border-dashed p-3 opacity-60': isRootUser,
        }"
      >
        <FormField
          field-name="enabled"
          layout="inline"
          :label="t('users.enabled')"
          :input-id="enabledId"
          :guide="isRootUser ? t('users.rootEnabledProtected') : undefined"
        >
          <FormSwitch
            :id="enabledId"
            v-model="enabled"
            :disabled="isRootUser"
            data-testid="user-editor-enabled"
          />
        </FormField>
      </div>
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
</template>
