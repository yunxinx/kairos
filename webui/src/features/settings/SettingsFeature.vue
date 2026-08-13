<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { Settings } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import FormField from '@/components/ui/FormField.vue';
import FormSwitch from '@/components/ui/FormSwitch.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import InlineError from '@/components/ui/InlineError.vue';
import SkeletonBlock from '@/components/ui/SkeletonBlock.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { formatBytesAsMb, parseMbToBytes } from '@/lib/format';

type SettingsSection = 'logging';

const { t } = useI18n();
const section = ref<SettingsSection>('logging');
const sections = computed(() => [{ id: 'logging' as const, labelKey: 'settings.section.logging' }]);
const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, clearErrors, validate } = useFormValidation();

const fullBody = ref(false);
const maxRequestMb = ref('');
/** 上次从接口载入的字节值；未改 MB 文案时原样回写，避免 0.00 显示把小字节上限抹掉。 */
const loadedMaxRequestBytes = ref<number | null>(null);
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
  loadedMaxRequestBytes.value = settings.max_request_bytes;
  maxRequestMb.value = formatBytesAsMb(settings.max_request_bytes);
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
  const loadedBytes = loadedMaxRequestBytes.value;
  const mbUnchanged =
    loadedBytes !== null && maxRequestMb.value.trim() === formatBytesAsMb(loadedBytes);
  if (!mbUnchanged) {
    const isValid = validate(
      [
        {
          name: 'maxRequestBytes',
          value: maxRequestMb.value,
          rules: [{ kind: 'required' }, { kind: 'mb', min: 1 }],
        },
      ],
      t,
    );
    if (!isValid) return;
  }
  const maxRequestBytes = mbUnchanged ? loadedBytes : parseMbToBytes(maxRequestMb.value);
  if (maxRequestBytes === null) return;
  saveMutation.mutate({
    full_body: fullBody.value,
    max_request_bytes: maxRequestBytes,
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
    <PageHeader :title="t('nav.settings')" />

    <InlineError
      v-if="settingsQuery.isError.value && !settingsQuery.data.value"
      :message="extractApiError(settingsQuery.error.value).message"
      @retry="() => settingsQuery.refetch()"
    />

    <form v-else novalidate @submit.prevent="handleSave">
      <div class="settings-layout">
        <nav class="card settings-nav" :aria-label="t('settings.sections')">
          <button
            v-for="item in sections"
            :key="item.id"
            type="button"
            class="settings-nav-item"
            :data-testid="'settings-section-' + item.id"
            :aria-current="section === item.id ? 'page' : undefined"
            @click="section = item.id"
          >
            {{ t(item.labelKey) }}
          </button>
        </nav>
        <div class="card settings-panel">
          <div
            v-if="settingsQuery.isPending.value && !settingsQuery.data.value"
            class="card-body space-y-4"
          >
            <SkeletonBlock height="h-4" width="w-40" />
            <SkeletonBlock height="h-10" width="w-full" />
            <SkeletonBlock height="h-4" width="w-48" />
            <SkeletonBlock height="h-10" width="w-full" />
          </div>
          <template v-else>
            <div v-if="section === 'logging'" class="card-body">
              <div class="settings-fields-row">
                <FormField
                  field-name="fullBody"
                  layout="inline"
                  :label="t('settings.fullBody')"
                  input-id="settings-full-body"
                  :guide="t('settings.fullBodyGuide')"
                >
                  <template #default>
                    <FormSwitch
                      id="settings-full-body"
                      v-model="fullBody"
                      data-testid="settings-full-body"
                    />
                  </template>
                </FormField>

                <FormField
                  field-name="maxRequestBytes"
                  layout="inline"
                  :label="t('settings.maxRequestBytes')"
                  input-id="settings-max-request-bytes"
                  :error="fieldError('maxRequestBytes')"
                  :guide="t('settings.maxRequestBytesGuide')"
                >
                  <template #default="{ hintId, invalid }">
                    <FormTextInput
                      id="settings-max-request-bytes"
                      v-model="maxRequestMb"
                      type="text"
                      inputmode="decimal"
                      class="font-mono"
                      data-testid="settings-max-request-bytes"
                      :invalid="invalid"
                      :hint-id="hintId"
                      v-on="fieldInputHandlers('maxRequestBytes')"
                    />
                  </template>
                </FormField>
              </div>
            </div>
            <div class="card-footer card-body flex flex-wrap items-center justify-end gap-2">
              <p
                v-if="saveError"
                class="text-danger mr-auto text-sm"
                data-testid="settings-save-error"
              >
                {{ saveError }}
              </p>
              <p
                v-if="saveSuccess"
                class="text-success mr-auto text-sm"
                data-testid="settings-save-success"
              >
                {{ t('settings.saveSuccess') }}
              </p>
              <button
                type="button"
                class="btn btn-subtle"
                data-testid="settings-reset"
                @click="resetForm"
              >
                {{ t('settings.reset') }}
              </button>
              <button
                type="submit"
                class="btn btn-primary"
                data-testid="settings-save"
                :disabled="saveMutation.isPending.value || !settingsQuery.data.value"
              >
                {{ t('settings.save') }}
              </button>
            </div>
          </template>
        </div>
      </div>
    </form>
  </div>
</template>
