<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { MeUpdate } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import FormField from '@/components/ui/FormField.vue';
import FormPasswordInput from '@/components/ui/FormPasswordInput.vue';
import FormSwitch from '@/components/ui/FormSwitch.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import InlineError from '@/components/ui/InlineError.vue';
import OverflowChips from '@/components/ui/OverflowChips.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { useToast } from '@/composables/useToast';
import { downscaleAvatar, isAcceptedAvatarType } from '@/lib/avatar';
import { formatDiscountBp, formatUsdMicros } from '@/lib/format';
import type { FieldValidationSpec } from '@/lib/form-validation';
import { useNavAvatarPreference, useNavNamePreference } from '@/lib/preferences';
import { captureSessionGeneration, setMeForSession, useCurrentUser } from '@/lib/session';

const { t } = useI18n();
const { error, success } = useToast();
const profileValidation = useFormValidation();
const passwordValidation = useFormValidation();
const me = useCurrentUser();
const { showNavAvatar } = useNavAvatarPreference();
const { showNavName } = useNavNamePreference();

const email = ref('');
const displayName = ref('');
const avatarData = ref<string | null>(null);
const fileInputRef = ref<HTMLInputElement | null>(null);

const currentPassword = ref('');
const newPassword = ref('');
const confirmPassword = ref('');
/** 改邮箱时验证身份用的当前密码；仅在邮箱实际变化时要求并提交。 */
const profileCurrentPassword = ref('');

watch(
  me,
  (user) => {
    if (!user) return;
    email.value = user.email;
    displayName.value = user.display_name;
    avatarData.value = user.avatar ?? null;
  },
  { immediate: true },
);

/** 仅 ASCII 小写：与服务端 normalize_email（to_ascii_lowercase）同一算法，
 * 避免 Unicode 大写（如 Á）导致前端判「已变化」而服务端 normalize 后判「未变」。 */
function asciiLowercase(value: string): string {
  return value.replace(/[A-Z]/g, (char) => char.toLowerCase());
}

/** 邮箱是唯一登录标识：实际变更时才需要当前密码（未改时隐藏输入框）。 */
const emailChanged = computed(
  () => !!me.value && asciiLowercase(email.value.trim()) !== me.value.email,
);

const roleLabel = computed(() => {
  const role = me.value?.role;
  if (role === 'root') return t('users.roleRoot');
  if (role === 'admin') return t('users.roleAdmin');
  return t('users.roleUser');
});

const roleBadgeClass = computed(() => {
  const role = me.value?.role;
  if (role === 'root') return 'badge-unified';
  if (role === 'admin') return 'badge-info';
  return 'badge-neutral';
});

const profileMutation = useMutation({
  mutationFn: (body: MeUpdate) => apiClient.updateMe(body),
  onSuccess: async () => {
    const generation = captureSessionGeneration();
    setMeForSession(await apiClient.getMe(), generation);
    profileCurrentPassword.value = '';
    success(t('account.saveSuccess'));
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

const passwordMutation = useMutation({
  mutationFn: (body: MeUpdate) => apiClient.updateMe(body),
  onSuccess: async () => {
    const generation = captureSessionGeneration();
    setMeForSession(await apiClient.getMe(), generation);
    currentPassword.value = '';
    newPassword.value = '';
    confirmPassword.value = '';
    success(t('account.passwordChanged'));
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

function triggerAvatarUpload() {
  fileInputRef.value?.click();
}

async function handleAvatarFileChange(e: Event) {
  const target = e.target as HTMLInputElement;
  const file = target.files?.[0];
  target.value = '';
  if (!file) return;
  if (!isAcceptedAvatarType(file.type)) {
    error(t('account.invalidImage'));
    return;
  }
  try {
    // 先在浏览器侧降采样：原图直传会把 MB 级 base64 写进库，也会被后端的
    // data URL 长度上限拒掉。
    const resized = await downscaleAvatar(file);
    avatarData.value = resized;
    profileMutation.mutate({ avatar: resized });
  } catch {
    error(t('account.invalidImage'));
  }
}

function handleRemoveAvatar() {
  avatarData.value = null;
  profileMutation.mutate({
    avatar: '',
  });
}

function handleSaveProfile() {
  const specs: FieldValidationSpec[] = [
    { name: 'email', value: email.value, rules: [{ kind: 'required' }] },
    { name: 'displayName', value: displayName.value, rules: [{ kind: 'required' }] },
  ];
  if (emailChanged.value) {
    specs.push({
      name: 'profileCurrentPassword',
      value: profileCurrentPassword.value,
      rules: [{ kind: 'required' }],
    });
  }
  if (!profileValidation.validate(specs, t)) return;
  profileMutation.mutate({
    ...(emailChanged.value
      ? { email: email.value.trim(), current_password: profileCurrentPassword.value }
      : {}),
    display_name: displayName.value.trim(),
  });
}

function handleChangePassword() {
  const specs: FieldValidationSpec[] = [
    { name: 'currentPassword', value: currentPassword.value, rules: [{ kind: 'required' }] },
    {
      name: 'newPassword',
      value: newPassword.value,
      rules: [{ kind: 'required' }, { kind: 'minLength', min: 8 }],
    },
    { name: 'confirmPassword', value: confirmPassword.value, rules: [{ kind: 'required' }] },
  ];
  if (!passwordValidation.validate(specs, t)) return;
  if (newPassword.value !== confirmPassword.value) {
    passwordValidation.showFieldError('confirmPassword', t('account.passwordMismatch'));
    return;
  }
  passwordMutation.mutate({
    password: newPassword.value,
    current_password: currentPassword.value,
  });
}
</script>

<template>
  <div class="w-full space-y-6">
    <PageHeader :title="t('nav.account')" />

    <InlineError v-if="!me" :message="t('account.missingProfile')" />

    <div v-else class="w-full space-y-6">
      <!-- 顶部个人画像与资产总览 Hero 卡片 -->
      <div class="card p-5 sm:p-6">
        <div class="flex flex-col gap-5 sm:flex-row sm:items-center sm:justify-between">
          <div class="flex items-center gap-4">
            <!-- 头像区域 -->
            <div class="group relative size-14 shrink-0">
              <div
                class="border-seed bg-surface-elevated text-fg-muted relative flex size-full items-center justify-center overflow-hidden rounded-full border text-xl font-bold shadow-sm"
              >
                <img
                  v-if="avatarData"
                  :src="avatarData"
                  alt="avatar"
                  class="size-full object-cover"
                  data-testid="account-avatar-img"
                />
                <UiIcon v-else name="user" class="size-7" />
              </div>
              <input
                ref="fileInputRef"
                type="file"
                accept="image/png,image/jpeg,image/webp,image/gif"
                aria-label="Upload avatar"
                class="hidden"
                data-testid="account-avatar-input"
                @change="handleAvatarFileChange"
              />
              <button
                type="button"
                class="absolute inset-0 flex cursor-pointer items-center justify-center rounded-full bg-black/50 text-white opacity-0 transition-opacity group-hover:opacity-100"
                data-testid="account-avatar-upload"
                :title="t('account.changeAvatar')"
                @click="triggerAvatarUpload"
              >
                <UiIcon name="pencil" class="size-4" />
              </button>
            </div>

            <div class="min-w-0">
              <div class="flex flex-wrap items-center gap-2">
                <h2 class="truncate text-lg font-bold tracking-tight">
                  {{ me.display_name || t('nav.account') }}
                </h2>
                <span class="badge" :class="roleBadgeClass">{{ roleLabel }}</span>
              </div>
              <p class="text-fg-muted mt-0.5 truncate font-mono text-xs">{{ me.email }}</p>
              <div v-if="avatarData" class="mt-2 flex items-center gap-2">
                <button
                  type="button"
                  class="btn btn-sm btn-ghost text-danger text-xs"
                  data-testid="account-remove-avatar-btn"
                  @click="handleRemoveAvatar"
                >
                  {{ t('account.removeAvatar') }}
                </button>
              </div>
            </div>
          </div>

          <!-- 余额总览与套餐/模型组指标 -->
          <div
            class="bg-surface-elevated border-seed flex shrink-0 items-center justify-between gap-6 rounded-md border px-5 py-3 sm:min-w-64"
          >
            <div>
              <p class="text-fg-muted text-xs font-medium">{{ t('account.balance') }}</p>
              <p class="font-mono text-xl font-bold tracking-tight">
                {{ formatUsdMicros(me.balance_usd_micros) }}
              </p>
            </div>
            <div v-if="me.role !== 'root'" class="text-right">
              <p class="text-fg-muted text-xs font-medium">{{ t('account.plan') }}</p>
              <p class="font-mono text-base font-semibold" data-testid="account-plan-name">
                {{ me.plan_display_name || '—' }}
              </p>
              <p class="text-fg-muted text-xs font-medium">{{ t('account.discount') }}</p>
              <p class="font-mono text-base font-semibold" data-testid="account-discount">
                {{ formatDiscountBp(me.discount_bp) }}
              </p>
            </div>
            <div v-else class="text-right">
              <p class="text-fg-muted text-xs font-medium">{{ t('account.plan') }}</p>
              <p class="font-mono text-base font-semibold">{{ t('common.unlimited') }}</p>
            </div>
            <div v-if="me.role !== 'root'" class="text-right">
              <p class="text-fg-muted text-xs font-medium">{{ t('account.assignedGroups') }}</p>
              <p class="font-mono text-base font-semibold">
                {{ me.assigned_groups.length }}
              </p>
            </div>
          </div>
        </div>

        <div
          v-if="me.role !== 'root' && me.assigned_groups.length > 0"
          class="border-seed mt-4 border-t pt-3"
        >
          <div class="flex flex-wrap items-center gap-2">
            <span class="text-fg-muted text-xs font-medium"
              >{{ t('account.assignedGroups') }}:</span
            >
            <div class="min-w-0 flex-1">
              <OverflowChips :items="me.assigned_groups" :threshold="6" class="inline-flex" />
            </div>
          </div>
        </div>
      </div>

      <!-- 双栏并排设定卡片 -->
      <div class="grid grid-cols-1 items-start gap-6 lg:grid-cols-2">
        <!-- 基本资料卡片 -->
        <form novalidate class="card flex flex-col" @submit.prevent="handleSaveProfile">
          <div class="card-body flex-1 space-y-4">
            <div class="border-seed border-b pb-3">
              <h3 class="text-sm font-semibold tracking-tight">{{ t('account.profile') }}</h3>
              <p class="text-fg-muted text-xs">{{ t('account.profileGuide') }}</p>
            </div>

            <FormField
              field-name="email"
              :label="t('account.email')"
              input-id="account-email"
              :error="profileValidation.fieldError('email')"
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
                  v-on="profileValidation.fieldInputHandlers('email')"
                />
              </template>
            </FormField>

            <FormField
              v-if="emailChanged"
              field-name="profileCurrentPassword"
              :label="t('account.emailChangePassword')"
              input-id="account-email-current-password"
              :error="profileValidation.fieldError('profileCurrentPassword')"
              :guide="t('account.emailChangePasswordGuide')"
            >
              <template #default="{ hintId, invalid }">
                <FormPasswordInput
                  id="account-email-current-password"
                  v-model="profileCurrentPassword"
                  autocomplete="current-password"
                  data-testid="account-email-current-password"
                  :invalid="invalid"
                  :hint-id="hintId"
                  v-on="profileValidation.fieldInputHandlers('profileCurrentPassword')"
                />
              </template>
            </FormField>

            <FormField
              field-name="displayName"
              :label="t('account.displayName')"
              input-id="account-display-name"
              :error="profileValidation.fieldError('displayName')"
            >
              <template #default="{ hintId, invalid }">
                <FormTextInput
                  id="account-display-name"
                  v-model="displayName"
                  autocomplete="nickname"
                  data-testid="account-display-name"
                  :invalid="invalid"
                  :hint-id="hintId"
                  v-on="profileValidation.fieldInputHandlers('displayName')"
                />
              </template>
            </FormField>

            <FormField
              field-name="showNavAvatar"
              layout="inline"
              :label="t('account.showNavAvatar')"
              :guide="t('account.showNavAvatarGuide')"
              input-id="account-show-nav-avatar"
            >
              <FormSwitch
                id="account-show-nav-avatar"
                v-model="showNavAvatar"
                data-testid="account-show-nav-avatar"
              />
            </FormField>

            <FormField
              field-name="showNavName"
              layout="inline"
              :label="t('account.showNavName')"
              :guide="t('account.showNavNameGuide')"
              input-id="account-show-nav-name"
            >
              <FormSwitch
                id="account-show-nav-name"
                v-model="showNavName"
                data-testid="account-show-nav-name"
              />
            </FormField>
          </div>

          <div class="card-footer card-body flex justify-end">
            <button
              type="submit"
              class="btn btn-primary"
              data-testid="account-save"
              :disabled="profileMutation.isPending.value"
            >
              {{ t('common.save') }}
            </button>
          </div>
        </form>

        <!-- 安全与修改密码卡片 -->
        <form novalidate class="card flex flex-col" @submit.prevent="handleChangePassword">
          <div class="card-body flex-1 space-y-4">
            <div class="border-seed border-b pb-3">
              <h3 class="text-sm font-semibold tracking-tight">{{ t('account.security') }}</h3>
              <p class="text-fg-muted text-xs">{{ t('account.securityGuide') }}</p>
            </div>

            <FormField
              field-name="currentPassword"
              :label="t('account.currentPassword')"
              input-id="account-current-password"
              :error="passwordValidation.fieldError('currentPassword')"
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
                  v-on="passwordValidation.fieldInputHandlers('currentPassword')"
                />
              </template>
            </FormField>

            <!-- 新密码与确认密码双栏排列 -->
            <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <FormField
                field-name="newPassword"
                :label="t('account.newPassword')"
                input-id="account-new-password"
                :error="passwordValidation.fieldError('newPassword')"
              >
                <template #default="{ hintId, invalid }">
                  <FormPasswordInput
                    id="account-new-password"
                    v-model="newPassword"
                    autocomplete="new-password"
                    data-testid="account-new-password"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="passwordValidation.fieldInputHandlers('newPassword')"
                  />
                </template>
              </FormField>

              <FormField
                field-name="confirmPassword"
                :label="t('account.confirmPassword')"
                input-id="account-confirm-password"
                :error="passwordValidation.fieldError('confirmPassword')"
              >
                <template #default="{ hintId, invalid }">
                  <FormPasswordInput
                    id="account-confirm-password"
                    v-model="confirmPassword"
                    autocomplete="new-password"
                    data-testid="account-confirm-password"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="passwordValidation.fieldInputHandlers('confirmPassword')"
                  />
                </template>
              </FormField>
            </div>
          </div>

          <div class="card-footer card-body flex justify-end">
            <button
              type="submit"
              class="btn"
              data-testid="account-password-save"
              :disabled="passwordMutation.isPending.value"
            >
              {{ t('account.changePassword') }}
            </button>
          </div>
        </form>
      </div>
    </div>
  </div>
</template>
