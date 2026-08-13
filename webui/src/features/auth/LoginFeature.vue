<script setup lang="ts">
import { ref } from 'vue';
import { useNavigate } from '@tanstack/vue-router';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import { setAdminKey } from '@/lib/session';
import MarketingSiteHeader from '@/app/shell/MarketingSiteHeader.vue';
import FormField from '@/components/ui/FormField.vue';
import FormPasswordInput from '@/components/ui/FormPasswordInput.vue';
import { useFormValidation } from '@/composables/useFormValidation';

const { t } = useI18n();
const navigate = useNavigate();
const { fieldError, fieldInputHandlers, validate } = useFormValidation();

const adminKey = ref('');
const loading = ref(false);
const errorMsg = ref('');

async function handleSubmit() {
  errorMsg.value = '';
  const isValid = validate(
    [{ name: 'adminKey', value: adminKey.value, rules: [{ kind: 'required' }] }],
    t,
  );
  if (!isValid) {
    return;
  }

  loading.value = true;
  try {
    await apiClient.listTokens(adminKey.value.trim());
    setAdminKey(adminKey.value.trim());
    await navigate({ to: '/overview' });
  } catch (err) {
    const extracted = extractApiError(err);
    errorMsg.value = extracted.code === 'unauthorized' ? t('auth.invalidKey') : extracted.message;
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
                field-name="adminKey"
                :label="t('auth.adminKey')"
                input-id="login-admin-key"
                :error="fieldError('adminKey')"
                :guide="t('fieldGuides.auth.adminKey')"
              >
                <template #default="{ hintId, invalid }">
                  <FormPasswordInput
                    id="login-admin-key"
                    v-model="adminKey"
                    autocomplete="off"
                    :placeholder="t('auth.adminKeyPlaceholder')"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('adminKey')"
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
