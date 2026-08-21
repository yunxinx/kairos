<script setup lang="ts">
import { ref } from 'vue';
import { useNavigate } from '@tanstack/vue-router';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import { setAdminKey, setMe } from '@/lib/session';
import MarketingSiteHeader from '@/app/shell/MarketingSiteHeader.vue';
import FormField from '@/components/ui/FormField.vue';
import FormPasswordInput from '@/components/ui/FormPasswordInput.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import { useFormValidation } from '@/composables/useFormValidation';

const { t } = useI18n();
const navigate = useNavigate();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();

// 登录卡对齐旧视觉：字段本身不挂 :guide。说明若需要，走标签行上的 FieldInfoHint 气泡，
// 不把提示文案铺在输入框下面，以免把登录卡撑成设置表单。

const email = ref('');
const password = ref('');
const loading = ref(false);
const errorMsg = ref('');

async function handleSubmit() {
  errorMsg.value = '';
  const isValid = validate(
    [
      { name: 'email', value: email.value, rules: [{ kind: 'required' }] },
      { name: 'password', value: password.value, rules: [{ kind: 'required' }] },
    ],
    t,
  );
  if (!isValid) {
    return;
  }

  loading.value = true;
  try {
    const loggedIn = await apiClient.login({
      email: email.value.trim(),
      password: password.value,
    });
    setAdminKey(loggedIn.token);
    setMe(await apiClient.getMe());
    await navigate({ to: '/overview' });
  } catch (err) {
    const extracted = extractApiError(err);
    errorMsg.value =
      extracted.code === 'unauthorized' ? t('auth.invalidCredentials') : extracted.message;
    loading.value = false;
  }
}
</script>

<template>
  <div class="dotted-bg flex min-h-0 flex-1 flex-col">
    <MarketingSiteHeader />
    <section class="marketing-main-stage px-4">
      <div class="card w-full max-w-sm">
        <div class="card-body">
          <h1 class="mb-6 text-center font-serif text-2xl font-normal">
            {{ t('auth.loginTitle') }}
          </h1>

          <form novalidate @submit.prevent="handleSubmit">
            <div class="space-y-4">
              <FormField
                field-name="email"
                :label="t('auth.email')"
                input-id="login-email"
                :error="fieldError('email')"
              >
                <template #default="{ hintId, invalid }">
                  <FormTextInput
                    id="login-email"
                    v-model="email"
                    type="email"
                    autocomplete="username"
                    :placeholder="t('auth.emailPlaceholder')"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('email')"
                  />
                </template>
              </FormField>
              <FormField
                field-name="password"
                :label="t('auth.password')"
                input-id="login-password"
                :error="fieldError('password')"
              >
                <template #default="{ hintId, invalid }">
                  <FormPasswordInput
                    id="login-password"
                    v-model="password"
                    autocomplete="current-password"
                    :placeholder="t('auth.passwordPlaceholder')"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('password')"
                  />
                </template>
              </FormField>
            </div>

            <p v-if="errorMsg" class="text-danger mt-4 text-sm">{{ errorMsg }}</p>

            <button type="submit" :disabled="loading" class="btn btn-primary mt-6 w-full">
              {{ loading ? t('auth.loggingIn') : t('auth.login') }}
            </button>
          </form>
        </div>
      </div>
    </section>
  </div>
</template>
