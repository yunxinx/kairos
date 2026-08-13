<script setup lang="ts">
import { ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { Settings } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import FormCheckbox from '@/components/ui/FormCheckbox.vue';
import FormField from '@/components/ui/FormField.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import InlineError from '@/components/ui/InlineError.vue';
import SkeletonBlock from '@/components/ui/SkeletonBlock.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { parseOptionalUint } from '@/lib/uint-parse';

const { t } = useI18n();
const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, clearErrors, validate } = useFormValidation();

const fullBody = ref(false);
const maxRequestBytes = ref('');
const saveError = ref('');
const saveSuccess = ref(false);

const settingsQuery = useQuery({
  queryKey: ['settings'],
  queryFn: () => apiClient.getSettings(),
});

watch(
  () => settingsQuery.data.value,
  (settings) => {
    if (!settings) return;
    applySettings(settings);
  },
  { immediate: true },
);

function applySettings(settings: Settings) {
  fullBody.value = settings.full_body;
  maxRequestBytes.value = String(settings.max_request_bytes);
}

const saveMutation = useMutation({
  mutationFn: (body: Settings) => apiClient.updateSettings(body),
  onSuccess: async () => {
    saveError.value = '';
    saveSuccess.value = true;
    await queryClient.invalidateQueries({ queryKey: ['settings'] });
    setTimeout(() => {
      saveSuccess.value = false;
    }, 4000);
  },
  onError: (err) => {
    saveSuccess.value = false;
    saveError.value = extractApiError(err).message;
  },
});

function handleSave() {
  saveError.value = '';
  saveSuccess.value = false;
  const isValid = validate(
    [
      {
        name: 'maxRequestBytes',
        value: maxRequestBytes.value,
        rules: [{ kind: 'required' }, { kind: 'uint' }],
      },
    ],
    t,
  );
  if (!isValid) return;
  const parsed = parseOptionalUint(maxRequestBytes.value);
  if (parsed === null) return;
  saveMutation.mutate({
    full_body: fullBody.value,
    max_request_bytes: parsed,
  });
}

function resetForm() {
  const settings = settingsQuery.data.value;
  if (!settings) return;
  clearErrors();
  saveError.value = '';
  saveSuccess.value = false;
  applySettings(settings);
}
</script>

<template>
  <div>
    <PageHeader :title="t('nav.settings')" :subtitle="t('settings.subtitle')" />

    <div v-if="settingsQuery.isPending.value" class="card">
      <div class="card-body space-y-3">
        <SkeletonBlock height="h-4" width="w-40" />
        <SkeletonBlock height="h-10" width="w-full" />
      </div>
    </div>

    <InlineError
      v-else-if="settingsQuery.isError.value"
      :message="extractApiError(settingsQuery.error.value).message"
      @retry="() => settingsQuery.refetch()"
    />

    <form v-else novalidate class="space-y-4" @submit.prevent="handleSave">
      <div class="card">
        <div class="card-body space-y-4">
          <FormField
            field-name="fullBody"
            :label="t('settings.fullBody')"
            input-id="settings-full-body"
            :guide="t('settings.fullBodyGuide')"
          >
            <template #default>
              <FormCheckbox
                id="settings-full-body"
                v-model="fullBody"
                data-testid="settings-full-body"
              />
            </template>
          </FormField>

          <FormField
            field-name="maxRequestBytes"
            :label="t('settings.maxRequestBytes')"
            input-id="settings-max-request-bytes"
            :error="fieldError('maxRequestBytes')"
            :guide="t('settings.maxRequestBytesGuide')"
          >
            <template #default="{ hintId, invalid }">
              <FormTextInput
                id="settings-max-request-bytes"
                v-model="maxRequestBytes"
                type="text"
                inputmode="numeric"
                data-testid="settings-max-request-bytes"
                :invalid="invalid"
                :hint-id="hintId"
                v-on="fieldInputHandlers('maxRequestBytes')"
              />
            </template>
          </FormField>
        </div>
      </div>

      <p v-if="saveError" class="text-danger text-sm" data-testid="settings-save-error">
        {{ saveError }}
      </p>
      <p v-if="saveSuccess" class="text-success text-sm" data-testid="settings-save-success">
        {{ t('settings.saveSuccess') }}
      </p>
      <div class="flex flex-wrap gap-2">
        <button
          type="submit"
          class="btn btn-primary"
          data-testid="settings-save"
          :disabled="saveMutation.isPending.value"
        >
          {{ t('settings.save') }}
        </button>
        <button
          type="button"
          class="btn btn-subtle"
          data-testid="settings-reset"
          @click="resetForm"
        >
          {{ t('settings.reset') }}
        </button>
      </div>
    </form>
  </div>
</template>
