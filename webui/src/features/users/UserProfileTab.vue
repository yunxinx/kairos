<script setup lang="ts">
import { useId, computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
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
import { hasCapability } from '@/lib/capabilities';
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
const emailId = `user-profile-email-${uid}`;
const planId = `user-profile-plan-${uid}`;

const initialEmail = props.user.email;
const initialName = props.user.display_name;
const initialRole = props.user.role;
const initialEnabled = props.user.enabled;
const initialRpm =
  props.user.rate_limit_rpm !== null && props.user.rate_limit_rpm !== undefined
    ? String(props.user.rate_limit_rpm)
    : '';
const initialPlanId = props.user.plan_id != null ? String(props.user.plan_id) : '';

const plansQuery = useQuery({
  queryKey: ['plans'],
  queryFn: () => apiClient.listPlans(),
});

const email = ref(initialEmail);
const displayName = ref(initialName);
const password = ref('');
const role = ref<ManagementRole>(initialRole);
const rateLimitRpm = ref(initialRpm);
const enabled = ref(initialEnabled);
const selectedPlanId = ref(initialPlanId);

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

const canAssignPlan = computed(() => hasCapability(me.value, 'assign_plan'));

function defaultPlanForRole(selectedRole: ManagementRole): string {
  if (selectedRole === 'root') return '';
  const audience = selectedRole === 'admin' ? 'admin' : 'user';
  const plan = (plansQuery.data.value ?? []).find(
    (candidate) => candidate.audience === audience && candidate.is_default,
  );
  return plan ? String(plan.id) : '';
}

const planOptions = computed(() => {
  const audience = role.value === 'admin' ? 'admin' : 'user';
  const options = (plansQuery.data.value ?? [])
    .filter((plan) => plan.audience === audience)
    .map((plan) => ({
      value: String(plan.id),
      label: plan.display_name,
    }));
  if (
    role.value === initialRole &&
    initialPlanId &&
    !options.some((option) => option.value === initialPlanId)
  ) {
    options.push({
      value: initialPlanId,
      label: props.user.plan_display_name || initialPlanId,
    });
  }
  return options;
});

watch(
  [role, plansQuery.data],
  ([selectedRole]) => {
    selectedPlanId.value =
      selectedRole === initialRole ? initialPlanId : defaultPlanForRole(selectedRole);
  },
  { immediate: true },
);

/** 仅 ASCII 小写：与服务端 normalize_email（to_ascii_lowercase）同一算法，
 * 避免 Unicode 大写（如 Á）导致前端判「已变化」而服务端 normalize 后判「未变」。 */
function asciiLowercase(value: string): string {
  return value.replace(/[A-Z]/g, (char) => char.toLowerCase());
}

const dirty = computed(
  () =>
    asciiLowercase(email.value.trim()) !== initialEmail ||
    displayName.value !== initialName ||
    password.value !== '' ||
    role.value !== initialRole ||
    enabled.value !== initialEnabled ||
    rateLimitRpm.value.trim() !== initialRpm ||
    selectedPlanId.value !== initialPlanId,
);
watch(dirty, (value) => emit('dirty-change', value), { immediate: true });

const saveMutation = useMutation({
  mutationFn: async () => {
    const parsedRpm = rateLimitRpm.value.trim() === '' ? null : Number(rateLimitRpm.value);
    const body: {
      email?: string;
      display_name?: string;
      role?: ManagementRole;
      enabled?: boolean;
      password?: string;
      rate_limit_rpm?: number | null;
      plan_id?: number;
    } = {
      display_name: displayName.value.trim(),
      enabled: enabled.value,
      rate_limit_rpm: parsedRpm,
    };
    // 邮箱只在实际变化时提交：改登录标识会吊销该用户的其他会话。
    if (asciiLowercase(email.value.trim()) !== initialEmail) {
      body.email = email.value.trim();
    }
    if (canPickRole.value) {
      body.role = role.value;
    }
    if (password.value) {
      body.password = password.value;
    }
    if (
      canAssignPlan.value &&
      selectedPlanId.value &&
      role.value === initialRole &&
      selectedPlanId.value !== initialPlanId
    ) {
      body.plan_id = Number(selectedPlanId.value);
    }
    const updated = await apiClient.updateUser(props.user.id, body);
    return updated;
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
  if (asciiLowercase(email.value.trim()) !== initialEmail) {
    specs.push({ name: 'email', value: email.value, rules: [{ kind: 'required' }] });
  }
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
        field-name="email"
        :label="t('users.email')"
        :input-id="emailId"
        :error="fieldError('email')"
        :guide="t('users.editEmailGuide')"
      >
        <template #default="{ hintId, invalid }">
          <FormTextInput
            :id="emailId"
            v-model="email"
            type="email"
            autocomplete="off"
            data-testid="user-profile-email"
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

      <FormField
        v-if="canAssignPlan && !isRootUser"
        field-name="plan"
        :label="t('users.plan')"
        :input-id="planId"
        :guide="t('users.planGuide')"
      >
        <UiSelect
          :id="planId"
          v-model="selectedPlanId"
          :options="planOptions"
          :disabled="role !== initialRole"
          data-testid="user-editor-plan"
        />
      </FormField>

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
