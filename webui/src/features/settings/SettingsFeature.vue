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
import UiSelect from '@/components/ui/UiSelect.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { useToast } from '@/composables/useToast';
import { formatBytesAsMb, formatCount, formatUnixMillis, parseMbToBytes } from '@/lib/format';

type SettingsSection = 'logging' | 'gateway' | 'catalog' | 'maintenance';

/** 日志清理窗口预设（天）：按占用量手动选档，不做自动周期。 */
const CLEANUP_WINDOWS = [7, 14, 30, 90, 180, 365] as const;

const { t, locale } = useI18n();
const { error, success } = useToast();
const section = ref<SettingsSection>('logging');
const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, clearErrors, showFieldError, validate } =
  useFormValidation();

const fullBody = ref(false);
const maxRequestMb = ref('');
const maxResponseMb = ref('');
const logBodyMb = ref('');
const catalogIntervalDays = ref('0');
const authThrottleMax = ref('');
const authThrottleWindow = ref('');
const sseReassemblyMb = ref('');
const retryBackoffMs = ref('');
const retryBackoffCapMs = ref('');
const retryAfterCapSecs = ref('');
const rateLimitRpm = ref('');
/** 上次从接口载入的字节值；未改 MB 文案时原样回写，避免 0.00 显示把小字节上限抹掉。 */
const loadedMaxRequestBytes = ref<number | null>(null);
const loadedMaxResponseBytes = ref<number | null>(null);
const loadedLogBodyBytes = ref<number | null>(null);
const loadedSseReassemblyBytes = ref<number | null>(null);

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
  loadedMaxResponseBytes.value = settings.max_response_bytes;
  maxResponseMb.value = formatBytesAsMb(settings.max_response_bytes);
  loadedLogBodyBytes.value = settings.log_body_max_bytes;
  logBodyMb.value = formatBytesAsMb(settings.log_body_max_bytes);
  catalogIntervalDays.value = String(settings.catalog_sync_interval_days);
  authThrottleMax.value = String(settings.auth_throttle_max_failures);
  authThrottleWindow.value = String(settings.auth_throttle_window_secs);
  loadedSseReassemblyBytes.value = settings.sse_reassembly_max_bytes;
  sseReassemblyMb.value = formatBytesAsMb(settings.sse_reassembly_max_bytes);
  retryBackoffMs.value = String(settings.retry_backoff_ms);
  retryBackoffCapMs.value = String(settings.retry_backoff_cap_ms);
  retryAfterCapSecs.value = String(settings.retry_after_cap_secs);
  rateLimitRpm.value = String(settings.rate_limit_rpm);
}

const saveMutation = useMutation({
  mutationFn: (body: Settings) => apiClient.updateSettings(body),
  onSuccess: async () => {
    success(t('settings.saveSuccess'));
    await queryClient.invalidateQueries({ queryKey: ['settings'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

function mbUnchanged(input: string, loaded: number | null): boolean {
  return loaded !== null && input.trim() === formatBytesAsMb(loaded);
}

function handleSave() {
  const requestMbSame = mbUnchanged(maxRequestMb.value, loadedMaxRequestBytes.value);
  const logBodyMbSame = mbUnchanged(logBodyMb.value, loadedLogBodyBytes.value);
  const sseMbSame = mbUnchanged(sseReassemblyMb.value, loadedSseReassemblyBytes.value);
  const responseMbSame = mbUnchanged(maxResponseMb.value, loadedMaxResponseBytes.value);
  if (!requestMbSame || !logBodyMbSame) {
    const isValid = validate(
      [
        ...(!requestMbSame
          ? [
              {
                name: 'maxRequestBytes',
                value: maxRequestMb.value,
                rules: [{ kind: 'required' as const }, { kind: 'mb' as const, min: 1 }],
              },
            ]
          : []),
        ...(!logBodyMbSame
          ? [
              {
                name: 'logBodyMaxBytes',
                value: logBodyMb.value,
                rules: [{ kind: 'required' as const }, { kind: 'mb' as const, min: 1 }],
              },
            ]
          : []),
      ],
      t,
    );
    if (!isValid) {
      section.value = 'logging';
      return;
    }
  }
  const maxRequestBytes = requestMbSame
    ? loadedMaxRequestBytes.value
    : parseMbToBytes(maxRequestMb.value);
  if (maxRequestBytes === null) return;
  const logBodyMaxBytes = logBodyMbSame
    ? loadedLogBodyBytes.value
    : parseMbToBytes(logBodyMb.value);
  if (logBodyMaxBytes === null) return;

  const gatewayValid = validate(
    [
      {
        name: 'authThrottleMax',
        value: authThrottleMax.value,
        rules: [{ kind: 'required' }, { kind: 'uint', min: 0 }],
      },
      {
        name: 'authThrottleWindow',
        value: authThrottleWindow.value,
        rules: [{ kind: 'required' }, { kind: 'uint', min: 1 }],
      },
      ...(!sseMbSame
        ? [
            {
              name: 'sseReassemblyMax',
              value: sseReassemblyMb.value,
              rules: [{ kind: 'required' as const }, { kind: 'mb' as const, min: 1 }],
            },
          ]
        : []),
      ...(!responseMbSame
        ? [
            {
              name: 'maxResponseBytes',
              value: maxResponseMb.value,
              rules: [{ kind: 'required' as const }, { kind: 'mb' as const, min: 1 }],
            },
          ]
        : []),
      {
        name: 'retryBackoffMs',
        value: retryBackoffMs.value,
        rules: [{ kind: 'required' }, { kind: 'uint', min: 1 }],
      },
      {
        name: 'retryBackoffCap',
        value: retryBackoffCapMs.value,
        rules: [{ kind: 'required' }, { kind: 'uint', min: 1 }],
      },
      {
        name: 'retryAfterCap',
        value: retryAfterCapSecs.value,
        rules: [{ kind: 'required' }, { kind: 'uint', min: 1 }],
      },
      {
        name: 'rateLimitRpm',
        value: rateLimitRpm.value,
        rules: [{ kind: 'required' }, { kind: 'uint', min: 0 }],
      },
    ],
    t,
  );
  if (!gatewayValid) {
    section.value = 'gateway';
    return;
  }
  const sseReassemblyMax = sseMbSame
    ? loadedSseReassemblyBytes.value
    : parseMbToBytes(sseReassemblyMb.value);
  if (sseReassemblyMax === null) return;
  const maxResponseBytes = responseMbSame
    ? loadedMaxResponseBytes.value
    : parseMbToBytes(maxResponseMb.value);
  if (maxResponseBytes === null) return;
  const backoffMs = Number(retryBackoffMs.value.trim());
  const backoffCapMs = Number(retryBackoffCapMs.value.trim());
  if (backoffCapMs < backoffMs) {
    section.value = 'gateway';
    showFieldError('retryBackoffCap', t('settings.retryBackoffCapTooSmall'));
    return;
  }

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
    max_response_bytes: maxResponseBytes,
    log_body_max_bytes: logBodyMaxBytes,
    catalog_sync_interval_days: Number(catalogIntervalDays.value.trim()),
    auth_throttle_max_failures: Number(authThrottleMax.value.trim()),
    auth_throttle_window_secs: Number(authThrottleWindow.value.trim()),
    sse_reassembly_max_bytes: sseReassemblyMax,
    retry_backoff_ms: backoffMs,
    retry_backoff_cap_ms: backoffCapMs,
    retry_after_cap_secs: Number(retryAfterCapSecs.value.trim()),
    rate_limit_rpm: Number(rateLimitRpm.value.trim()),
  });
}

function resetForm() {
  const settings = settingsQuery.data.value;
  if (!settings) return;
  clearErrors();
  applySettings(settings);
}

const syncMutation = useMutation({
  mutationFn: () => apiClient.syncCatalog(),
  onSuccess: async () => {
    await queryClient.invalidateQueries({ queryKey: ['catalog'] });
    await queryClient.invalidateQueries({ queryKey: ['catalog-meta'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

function syncedLabel(meta: CatalogMeta | undefined): string {
  if (!meta?.synced_at) return t('settings.catalogNeverSynced');
  return formatUnixMillis(meta.synced_at);
}

// --- 日志维护（root-only 页签；端点同样只对 root 开放） ---

const logsSizeQuery = useQuery({
  queryKey: ['logs-size'],
  queryFn: () => apiClient.getLogsSize(),
});

const cleanupWindowDays = ref('30');
/** 两步确认：false 普通态，true 为已展示警告待确认。 */
const cleanupConfirming = ref(false);
watch(cleanupWindowDays, () => {
  cleanupConfirming.value = false;
});

const cleanupMutation = useMutation({
  mutationFn: (days: number) => apiClient.cleanupLogs(days),
  onSuccess: async (result) => {
    cleanupConfirming.value = false;
    success(
      t('settings.maintenanceDone', {
        requests: result.removed_request_logs,
        system: result.removed_system_logs,
      }),
    );
    // 删除的行直接改变统计口径：把所有受影响的查询缓存一并失效，否则 30 秒
    // staleTime 内概览/用户/日志页还会展示已删除的数据，与「已删除明细不再
    // 计入统计」的文案自相矛盾。前缀失效覆盖各自的全部参数化键。
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ['logs-size'] }),
      queryClient.invalidateQueries({ queryKey: ['stats'] }),
      queryClient.invalidateQueries({ queryKey: ['users'] }),
      queryClient.invalidateQueries({ queryKey: ['logs'] }),
      queryClient.invalidateQueries({ queryKey: ['system-logs'] }),
    ]);
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});

function handleCleanup() {
  if (!cleanupConfirming.value) {
    cleanupConfirming.value = true;
    return;
  }
  cleanupMutation.mutate(Number(cleanupWindowDays.value));
}

const windowOptions = computed(() =>
  CLEANUP_WINDOWS.map((days) => ({
    value: String(days),
    label: t('settings.maintenanceWindowOption', { days }),
  })),
);

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
            value="gateway"
            class="page-tab-switch-btn"
            data-testid="settings-section-gateway"
          >
            {{ t('settings.section.gateway') }}
          </TabsTrigger>
          <TabsTrigger
            value="catalog"
            class="page-tab-switch-btn"
            data-testid="settings-section-catalog"
          >
            {{ t('settings.section.catalog') }}
          </TabsTrigger>
          <TabsTrigger
            value="maintenance"
            class="page-tab-switch-btn"
            data-testid="settings-section-maintenance"
          >
            {{ t('settings.section.maintenance') }}
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
              <FormField
                field-name="logBodyMaxBytes"
                layout="inline"
                :label="t('settings.logBodyMaxBytes')"
                input-id="settings-log-body-max-bytes"
                :error="fieldError('logBodyMaxBytes')"
                :guide="t('settings.logBodyMaxBytesGuide')"
              >
                <template #default="{ hintId, invalid }">
                  <FormTextInput
                    id="settings-log-body-max-bytes"
                    v-model="logBodyMb"
                    type="text"
                    inputmode="decimal"
                    class="font-mono"
                    data-testid="settings-log-body-max-bytes"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('logBodyMaxBytes')"
                  />
                </template>
              </FormField>
            </div>
          </TabsContent>
          <TabsContent value="gateway" class="card-body">
            <div class="settings-fields-row">
              <FormField
                field-name="authThrottleMax"
                layout="inline"
                :label="t('settings.authThrottleMax')"
                input-id="settings-auth-throttle-max"
                :error="fieldError('authThrottleMax')"
                :guide="t('settings.authThrottleMaxGuide')"
              >
                <template #default="{ hintId, invalid }">
                  <FormTextInput
                    id="settings-auth-throttle-max"
                    v-model="authThrottleMax"
                    type="text"
                    inputmode="numeric"
                    class="font-mono"
                    data-testid="settings-auth-throttle-max"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('authThrottleMax')"
                  />
                </template>
              </FormField>
              <FormField
                field-name="authThrottleWindow"
                layout="inline"
                :label="t('settings.authThrottleWindow')"
                input-id="settings-auth-throttle-window"
                :error="fieldError('authThrottleWindow')"
                :guide="t('settings.authThrottleWindowGuide')"
              >
                <template #default="{ hintId, invalid }">
                  <FormTextInput
                    id="settings-auth-throttle-window"
                    v-model="authThrottleWindow"
                    type="text"
                    inputmode="numeric"
                    class="font-mono"
                    data-testid="settings-auth-throttle-window"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('authThrottleWindow')"
                  />
                </template>
              </FormField>
              <FormField
                field-name="maxResponseBytes"
                layout="inline"
                :label="t('settings.maxResponseBytes')"
                input-id="settings-max-response-bytes"
                :error="fieldError('maxResponseBytes')"
                :guide="t('settings.maxResponseBytesGuide')"
              >
                <template #default="{ hintId, invalid }">
                  <FormTextInput
                    id="settings-max-response-bytes"
                    v-model="maxResponseMb"
                    type="text"
                    inputmode="decimal"
                    class="font-mono"
                    data-testid="settings-max-response-bytes"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('maxResponseBytes')"
                  />
                </template>
              </FormField>
              <FormField
                field-name="sseReassemblyMax"
                layout="inline"
                :label="t('settings.sseReassemblyMax')"
                input-id="settings-sse-reassembly-max"
                :error="fieldError('sseReassemblyMax')"
                :guide="t('settings.sseReassemblyMaxGuide')"
              >
                <template #default="{ hintId, invalid }">
                  <FormTextInput
                    id="settings-sse-reassembly-max"
                    v-model="sseReassemblyMb"
                    type="text"
                    inputmode="decimal"
                    class="font-mono"
                    data-testid="settings-sse-reassembly-max"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('sseReassemblyMax')"
                  />
                </template>
              </FormField>
              <FormField
                field-name="retryBackoffMs"
                layout="inline"
                :label="t('settings.retryBackoffMs')"
                input-id="settings-retry-backoff-ms"
                :error="fieldError('retryBackoffMs')"
                :guide="t('settings.retryBackoffMsGuide')"
              >
                <template #default="{ hintId, invalid }">
                  <FormTextInput
                    id="settings-retry-backoff-ms"
                    v-model="retryBackoffMs"
                    type="text"
                    inputmode="numeric"
                    class="font-mono"
                    data-testid="settings-retry-backoff-ms"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('retryBackoffMs')"
                  />
                </template>
              </FormField>
              <FormField
                field-name="retryBackoffCap"
                layout="inline"
                :label="t('settings.retryBackoffCap')"
                input-id="settings-retry-backoff-cap"
                :error="fieldError('retryBackoffCap')"
                :guide="t('settings.retryBackoffCapGuide')"
              >
                <template #default="{ hintId, invalid }">
                  <FormTextInput
                    id="settings-retry-backoff-cap"
                    v-model="retryBackoffCapMs"
                    type="text"
                    inputmode="numeric"
                    class="font-mono"
                    data-testid="settings-retry-backoff-cap"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('retryBackoffCap')"
                  />
                </template>
              </FormField>
              <FormField
                field-name="retryAfterCap"
                layout="inline"
                :label="t('settings.retryAfterCap')"
                input-id="settings-retry-after-cap"
                :error="fieldError('retryAfterCap')"
                :guide="t('settings.retryAfterCapGuide')"
              >
                <template #default="{ hintId, invalid }">
                  <FormTextInput
                    id="settings-retry-after-cap"
                    v-model="retryAfterCapSecs"
                    type="text"
                    inputmode="numeric"
                    class="font-mono"
                    data-testid="settings-retry-after-cap"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('retryAfterCap')"
                  />
                </template>
              </FormField>
              <FormField
                field-name="rateLimitRpm"
                layout="inline"
                :label="t('settings.rateLimitRpm')"
                input-id="settings-rate-limit-rpm"
                :error="fieldError('rateLimitRpm')"
                :guide="t('settings.rateLimitRpmGuide')"
              >
                <template #default="{ hintId, invalid }">
                  <FormTextInput
                    id="settings-rate-limit-rpm"
                    v-model="rateLimitRpm"
                    type="text"
                    inputmode="numeric"
                    class="font-mono"
                    data-testid="settings-rate-limit-rpm"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('rateLimitRpm')"
                  />
                </template>
              </FormField>
            </div>
          </TabsContent>
          <TabsContent value="maintenance" class="card-body">
            <InlineError
              v-if="logsSizeQuery.isError.value && !logsSizeQuery.data.value"
              :message="extractApiError(logsSizeQuery.error.value).message"
              @retry="() => logsSizeQuery.refetch()"
            />
            <div v-else class="settings-fields-row">
              <FormField
                field-name="maintenanceSize"
                layout="inline"
                :label="t('settings.maintenanceSize')"
                input-id="settings-maintenance-size"
              >
                <div class="flex flex-wrap items-center gap-2">
                  <p
                    id="settings-maintenance-size"
                    class="font-mono text-sm"
                    data-testid="settings-maintenance-size"
                    :title="
                      logsSizeQuery.data.value
                        ? t('settings.maintenanceSizeBreakdown', {
                            main: formatBytesAsMb(logsSizeQuery.data.value.db_size_bytes),
                            wal: formatBytesAsMb(logsSizeQuery.data.value.wal_size_bytes),
                          })
                        : undefined
                    "
                  >
                    {{
                      logsSizeQuery.data.value
                        ? `${formatBytesAsMb(
                            logsSizeQuery.data.value.db_size_bytes +
                              logsSizeQuery.data.value.wal_size_bytes,
                          )} MB`
                        : '—'
                    }}
                  </p>
                  <button
                    type="button"
                    class="btn inline-flex items-center gap-1.5"
                    data-testid="settings-maintenance-refresh"
                    :disabled="logsSizeQuery.isFetching.value"
                    @click="() => logsSizeQuery.refetch()"
                  >
                    <UiIcon
                      v-if="logsSizeQuery.isFetching.value"
                      name="loader-circle"
                      :size="14"
                      class="animate-spin"
                    />
                    {{ t('settings.maintenanceRefresh') }}
                  </button>
                </div>
              </FormField>
              <FormField
                field-name="maintenanceRequestRows"
                layout="inline"
                :label="t('settings.maintenanceRequestRows')"
                input-id="settings-maintenance-request-rows"
              >
                <p
                  id="settings-maintenance-request-rows"
                  class="font-mono text-sm"
                  data-testid="settings-maintenance-request-rows"
                >
                  {{
                    logsSizeQuery.data.value
                      ? formatCount(logsSizeQuery.data.value.request_log_rows, locale)
                      : '—'
                  }}
                </p>
              </FormField>
              <FormField
                field-name="maintenanceSystemRows"
                layout="inline"
                :label="t('settings.maintenanceSystemRows')"
                input-id="settings-maintenance-system-rows"
              >
                <p
                  id="settings-maintenance-system-rows"
                  class="font-mono text-sm"
                  data-testid="settings-maintenance-system-rows"
                >
                  {{
                    logsSizeQuery.data.value
                      ? formatCount(logsSizeQuery.data.value.system_log_rows, locale)
                      : '—'
                  }}
                </p>
              </FormField>
              <FormField
                field-name="maintenanceWindow"
                layout="inline"
                :label="t('settings.maintenanceWindow')"
                input-id="settings-maintenance-window"
                :guide="t('settings.maintenanceWindowGuide')"
              >
                <div class="flex flex-wrap items-center gap-2">
                  <UiSelect
                    id="settings-maintenance-window"
                    v-model="cleanupWindowDays"
                    :options="windowOptions"
                    :disabled="cleanupMutation.isPending.value"
                    data-testid="settings-maintenance-window"
                  />
                  <template v-if="!cleanupConfirming">
                    <button
                      type="button"
                      class="btn btn-danger-filled"
                      data-testid="settings-maintenance-cleanup"
                      :disabled="cleanupMutation.isPending.value"
                      @click="handleCleanup"
                    >
                      {{ t('settings.maintenanceAction') }}
                    </button>
                  </template>
                  <template v-else>
                    <p
                      class="text-danger text-xs"
                      data-testid="settings-maintenance-warning"
                    >
                      {{ t('settings.maintenanceWarning', { days: Number(cleanupWindowDays) }) }}
                    </p>
                    <button
                      type="button"
                      class="btn"
                      data-testid="settings-maintenance-cancel"
                      @click="cleanupConfirming = false"
                    >
                      {{ t('common.cancel') }}
                    </button>
                    <button
                      type="button"
                      class="btn btn-danger-filled"
                      data-testid="settings-maintenance-confirm"
                      :disabled="cleanupMutation.isPending.value"
                      @click="handleCleanup"
                    >
                      {{ t('common.confirm') }}
                    </button>
                  </template>
                </div>
              </FormField>
            </div>
          </TabsContent>
          <div class="card-footer card-body flex flex-wrap items-center justify-end gap-2">
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
