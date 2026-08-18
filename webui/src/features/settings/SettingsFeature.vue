<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { TabsContent, TabsIndicator, TabsList, TabsRoot, TabsTrigger } from 'reka-ui';
import { apiClient, extractApiError } from '@/api/client';
import type { CatalogMeta, Settings } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import FormField from '@/components/ui/FormField.vue';
import FormSwitch from '@/components/ui/FormSwitch.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import InlineError from '@/components/ui/InlineError.vue';
import SkeletonBlock from '@/components/ui/SkeletonBlock.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { formatBytesAsMb, formatUnixMillis, parseMbToBytes } from '@/lib/format';

type SettingsSection = 'logging' | 'catalog';

const { t } = useI18n();
const section = ref<SettingsSection>('logging');
const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, clearErrors, validate } = useFormValidation();

const fullBody = ref(false);
const maxRequestMb = ref('');
const catalogIntervalDays = ref('0');
/** 上次从接口载入的字节值；未改 MB 文案时原样回写，避免 0.00 显示把小字节上限抹掉。 */
const loadedMaxRequestBytes = ref<number | null>(null);
const saveError = ref('');
const saveSuccess = ref(false);
const syncError = ref('');

const settingsQuery = useQuery({
  queryKey: ['settings'],
  queryFn: () => apiClient.getSettings(),
});

const catalogMetaQuery = useQuery({
  queryKey: ['catalog-meta'],
  queryFn: () => apiClient.getCatalogMeta(),
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
  catalogIntervalDays.value = String(settings.catalog_sync_interval_days);
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
    if (!isValid) {
      section.value = 'logging';
      return;
    }
  }
  const maxRequestBytes = mbUnchanged ? loadedBytes : parseMbToBytes(maxRequestMb.value);
  if (maxRequestBytes === null) return;
  const intervalValid = validate(
    [
      {
        name: 'catalogInterval',
        value: catalogIntervalDays.value,
        rules: [{ kind: 'required' }, { kind: 'uint', min: 0 }],
      },
    ],
    t,
  );
  if (!intervalValid) {
    section.value = 'catalog';
    return;
  }
  saveMutation.mutate({
    full_body: fullBody.value,
    max_request_bytes: maxRequestBytes,
    catalog_sync_interval_days: Number(catalogIntervalDays.value.trim()),
  });
}

function resetForm() {
  const settings = settingsQuery.data.value;
  if (!settings) return;
  clearErrors();
  saveError.value = '';
  saveSuccess.value = false;
  syncError.value = '';
  applySettings(settings);
}

const syncMutation = useMutation({
  mutationFn: () => apiClient.syncCatalog(),
  onSuccess: async () => {
    syncError.value = '';
    await queryClient.invalidateQueries({ queryKey: ['catalog'] });
    await queryClient.invalidateQueries({ queryKey: ['catalog-meta'] });
  },
  onError: (err) => {
    syncError.value = extractApiError(err).message;
  },
});

function syncedLabel(meta: CatalogMeta | undefined): string {
  if (!meta?.synced_at) return t('settings.catalogNeverSynced');
  return formatUnixMillis(meta.synced_at);
}

const tabsAria = computed(() => t('settings.sections'));
</script>

<template>
  <TabsRoot v-model="section" class="flex flex-col">
    <PageHeader>
      <template #leading>
        <TabsList class="page-tab-switch" :aria-label="tabsAria">
          <TabsIndicator class="page-tab-switch-knob" />
          <TabsTrigger
            value="logging"
            class="page-tab-switch-btn"
            data-testid="settings-section-logging"
          >
            {{ t('settings.section.logging') }}
          </TabsTrigger>
          <TabsTrigger
            value="catalog"
            class="page-tab-switch-btn"
            data-testid="settings-section-catalog"
          >
            {{ t('settings.section.catalog') }}
          </TabsTrigger>
        </TabsList>
      </template>
    </PageHeader>

    <InlineError
      v-if="settingsQuery.isError.value && !settingsQuery.data.value"
      :message="extractApiError(settingsQuery.error.value).message"
      @retry="() => settingsQuery.refetch()"
    />

    <form v-else novalidate @submit.prevent="handleSave">
      <div class="card">
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
          <TabsContent value="catalog" class="card-body">
            <div class="settings-fields-row">
              <FormField
                field-name="catalogSyncedAt"
                layout="inline"
                :label="t('settings.catalogSyncedAt')"
                input-id="settings-catalog-synced-at"
                :guide="t('settings.catalogSyncedAtGuide')"
              >
                <div class="flex flex-wrap items-center gap-2">
                  <p
                    id="settings-catalog-synced-at"
                    class="text-sm"
                    data-testid="settings-catalog-synced-at"
                  >
                    {{ syncedLabel(catalogMetaQuery.data.value) }}
                  </p>
                  <button
                    type="button"
                    class="btn inline-flex items-center gap-1.5"
                    data-testid="settings-catalog-sync"
                    :disabled="syncMutation.isPending.value"
                    @click="syncMutation.mutate()"
                  >
                    <UiIcon
                      v-if="syncMutation.isPending.value"
                      name="loader-circle"
                      :size="14"
                      class="animate-spin"
                    />
                    {{
                      syncMutation.isPending.value
                        ? t('settings.catalogSyncing')
                        : t('settings.catalogSyncNow')
                    }}
                  </button>
                </div>
              </FormField>
              <FormField
                field-name="catalogInterval"
                layout="inline"
                :label="t('settings.catalogInterval')"
                input-id="settings-catalog-interval"
                :error="fieldError('catalogInterval')"
                :guide="t('settings.catalogIntervalGuide')"
              >
                <template #default="{ hintId, invalid }">
                  <FormTextInput
                    id="settings-catalog-interval"
                    v-model="catalogIntervalDays"
                    type="text"
                    inputmode="numeric"
                    class="font-mono"
                    data-testid="settings-catalog-interval"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('catalogInterval')"
                  />
                </template>
              </FormField>
            </div>
            <p
              v-if="syncError"
              class="text-danger mt-3 text-sm"
              data-testid="settings-catalog-sync-error"
            >
              {{ syncError }}
            </p>
          </TabsContent>
          <TabsContent value="logging" class="card-body">
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
          </TabsContent>
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
    </form>
  </TabsRoot>
</template>
