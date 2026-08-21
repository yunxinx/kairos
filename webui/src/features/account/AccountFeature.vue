<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { MeUpdate } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import FormField from '@/components/ui/FormField.vue';
import FormPasswordInput from '@/components/ui/FormPasswordInput.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import InlineError from '@/components/ui/InlineError.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { useToast } from '@/composables/useToast';
import type { FieldValidationSpec } from '@/lib/form-validation';
import { setMe, useCurrentUser } from '@/lib/session';

const { t } = useI18n();
const { error, success } = useToast();
const { fieldError, fieldInputHandlers, validate, showFieldError } = useFormValidation();
const me = useCurrentUser();

const email = ref('');
const displayName = ref('');
const currentPassword = ref('');
const newPassword = ref('');
const confirmPassword = ref('');

watch(
  me,
  (user) => {
    if (!user) return;
    email.value = user.email;
    displayName.value = user.display_name;
  },
  { immediate: true },
);

// 只改邮箱/名称不必带当前密码；三个密码框任一有字才走改密，后端会再校验 current_password。
const changingPassword = computed(
  () =>
    currentPassword.value.length > 0 ||
    newPassword.value.length > 0 ||
    confirmPassword.value.length > 0,
);

const saveMutation = useMutation({
  mutationFn: (body: MeUpdate) => apiClient.updateMe(body),
  onSuccess: async () => {
    setMe(await apiClient.getMe());
    currentPassword.value = '';
    newPassword.value = '';
    confirmPassword.value = '';
    success(t('account.saveSuccess'));
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

function handleSave() {
  const specs: FieldValidationSpec[] = [
    { name: 'email', value: email.value, rules: [{ kind: 'required' }] },
    { name: 'displayName', value: displayName.value, rules: [{ kind: 'required' }] },
  ];
  if (changingPassword.value) {
    specs.push(
      { name: 'currentPassword', value: currentPassword.value, rules: [{ kind: 'required' }] },
      {
        name: 'newPassword',
        value: newPassword.value,
        rules: [{ kind: 'required' }, { kind: 'minLength', min: 8 }],
      },
      { name: 'confirmPassword', value: confirmPassword.value, rules: [{ kind: 'required' }] },
    );
  }
  if (!validate(specs, t)) return;
  if (changingPassword.value && newPassword.value !== confirmPassword.value) {
    showFieldError('confirmPassword', t('account.passwordMismatch'));
    return;
  }
  const body: MeUpdate = {
    email: email.value.trim(),
    display_name: displayName.value.trim(),
  };
  if (changingPassword.value) {
    body.password = newPassword.value;
    body.current_password = currentPassword.value;
  }
  saveMutation.mutate(body);
}
</script>

<template>
  <div class="flex flex-col">
    <PageHeader :title="t('nav.account')" />

    <InlineError v-if="!me" :message="t('account.missingProfile')" />

    <form v-else novalidate class="max-w-xl" @submit.prevent="handleSave">
      <div class="card">
        <div class="card-body space-y-4">
          <FormField
            field-name="email"
            :label="t('account.email')"
            input-id="account-email"
            :error="fieldError('email')"
            :guide="t('account.emailGuide')"
          >
            <template #default="{ hintId, invalid }">
              <FormTextInput
                id="account-email"
                v-model="email"
                type="email"
                autocomplete="email"
                data-testid="account-email"
                :invalid="invalid"
                :hint-id="hintId"
                v-on="fieldInputHandlers('email')"
              />
            </template>
          </FormField>
          <FormField
            field-name="displayName"
            :label="t('account.displayName')"
            input-id="account-display-name"
            :error="fieldError('displayName')"
          >
            <template #default="{ hintId, invalid }">
              <FormTextInput
                id="account-display-name"
                v-model="displayName"
                autocomplete="nickname"
                data-testid="account-display-name"
                :invalid="invalid"
                :hint-id="hintId"
                v-on="fieldInputHandlers('displayName')"
              />
            </template>
          </FormField>
          <FormField
            field-name="currentPassword"
            :label="t('account.currentPassword')"
            input-id="account-current-password"
            :error="fieldError('currentPassword')"
            :guide="t('account.currentPasswordGuide')"
          >
            <template #default="{ hintId, invalid }">
              <FormPasswordInput
                id="account-current-password"
                v-model="currentPassword"
                autocomplete="current-password"
                data-testid="account-current-password"
                :invalid="invalid"
                :hint-id="hintId"
                v-on="fieldInputHandlers('currentPassword')"
              />
            </template>
          </FormField>
          <FormField
            field-name="newPassword"
            :label="t('account.newPassword')"
            input-id="account-new-password"
            :error="fieldError('newPassword')"
            :guide="t('users.passwordGuide')"
          >
            <template #default="{ hintId, invalid }">
              <FormPasswordInput
                id="account-new-password"
                v-model="newPassword"
                autocomplete="new-password"
                data-testid="account-new-password"
                :invalid="invalid"
                :hint-id="hintId"
                v-on="fieldInputHandlers('newPassword')"
              />
            </template>
          </FormField>
          <FormField
            field-name="confirmPassword"
            :label="t('account.confirmPassword')"
            input-id="account-confirm-password"
            :error="fieldError('confirmPassword')"
          >
            <template #default="{ hintId, invalid }">
              <FormPasswordInput
                id="account-confirm-password"
                v-model="confirmPassword"
                autocomplete="new-password"
                data-testid="account-confirm-password"
                :invalid="invalid"
                :hint-id="hintId"
                v-on="fieldInputHandlers('confirmPassword')"
              />
            </template>
          </FormField>
        </div>
        <div class="card-footer card-body flex justify-end">
          <button
            type="submit"
            class="btn btn-primary"
            data-testid="account-save"
            :disabled="saveMutation.isPending.value"
          >
            {{ t('common.save') }}
          </button>
        </div>
      </div>
    </form>
  </div>
</template>
