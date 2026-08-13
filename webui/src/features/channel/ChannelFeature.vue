<script setup lang="ts">
import { computed, ref, useId } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { Channel, ChannelProbeResult, Protocol } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import AppModal from '@/components/ui/AppModal.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FormField from '@/components/ui/FormField.vue';
import FormPasswordInput from '@/components/ui/FormPasswordInput.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import FormTextarea from '@/components/ui/FormTextarea.vue';
import InlineError from '@/components/ui/InlineError.vue';
import DataTable from '@/components/ui/data-table/DataTable.vue';
import DataTableMenuItem from '@/components/ui/data-table/DataTableMenuItem.vue';
import DataTableMenuSeparator from '@/components/ui/data-table/DataTableMenuSeparator.vue';
import DataTableRowActions from '@/components/ui/data-table/DataTableRowActions.vue';
import DataTableToolbar from '@/components/ui/data-table/DataTableToolbar.vue';
import UiSelect from '@/components/ui/UiSelect.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { parseOptionalUint } from '@/lib/uint-parse';
import type { FieldValidationSpec } from '@/lib/form-validation';

const PROTOCOLS: Protocol[] = ['openai_chat', 'openai_responses', 'anthropic_messages'];

const { t } = useI18n();
const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, clearErrors, validate } = useFormValidation();

const showEditor = ref(false);
const editingName = ref<string | null>(null);
const editorName = ref('');
const editorProtocol = ref<Protocol>('openai_chat');
const editorBaseUrl = ref('');
const editorApiKey = ref('');
const editorModels = ref('');
const editorAliases = ref('');
const editorPriority = ref('0');
const editorWeight = ref('1');
const editorTimeoutMs = ref('30000');
const editorMaxRetries = ref('0');
const editorError = ref('');
const editorTitleId = useId();

const confirmingDeleteName = ref<string | null>(null);
const actionError = ref('');
const probeByName = ref<Record<string, ChannelProbeResult>>({});
const testingName = ref<string | null>(null);
const searchText = ref('');

const protocolOptions = computed(() =>
  PROTOCOLS.map((value) => ({
    value,
    label: t(`protocol.${value}`),
  })),
);

const channelsQuery = useQuery({
  queryKey: ['channels'],
  queryFn: () => apiClient.listChannels(),
});

const channels = computed(() => channelsQuery.data.value ?? []);
const showTableSkeleton = computed(
  () => channelsQuery.isPending.value && !channelsQuery.data.value,
);

const filteredChannels = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  if (!q) return channels.value;
  return channels.value.filter((channel) => {
    if (channel.name.toLowerCase().includes(q)) return true;
    if (channel.base_url.toLowerCase().includes(q)) return true;
    if (channel.models.some((model) => model.toLowerCase().includes(q))) return true;
    return t(`protocol.${channel.protocol}`).toLowerCase().includes(q);
  });
});

function invalidateChannels() {
  return queryClient.invalidateQueries({ queryKey: ['channels'] });
}

function parseModelList(text: string): string[] {
  return text
    .split(/[\n,]/)
    .map((item) => item.trim())
    .filter((item) => item.length > 0);
}

function formatModelList(models: string[]): string {
  return models.join('\n');
}

function parseAliases(text: string): Record<string, string> | null {
  const aliases: Record<string, string> = {};
  for (const line of text.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const eq = trimmed.indexOf('=');
    if (eq <= 0 || eq === trimmed.length - 1) {
      return null;
    }
    const alias = trimmed.slice(0, eq).trim();
    const canonical = trimmed.slice(eq + 1).trim();
    if (!alias || !canonical) {
      return null;
    }
    aliases[alias] = canonical;
  }
  return aliases;
}

function formatAliases(aliases: Record<string, string>): string {
  return Object.entries(aliases)
    .map(([alias, canonical]) => `${alias}=${canonical}`)
    .join('\n');
}

const saveMutation = useMutation({
  mutationFn: (body: Channel) =>
    editingName.value === null
      ? apiClient.createChannel(body)
      : apiClient.updateChannel(editingName.value, body),
  onSuccess: async () => {
    editorError.value = '';
    showEditor.value = false;
    await invalidateChannels();
  },
  onError: (err) => {
    editorError.value = extractApiError(err).message;
  },
});

const deleteMutation = useMutation({
  mutationFn: (name: string) => apiClient.deleteChannel(name),
  onSuccess: async () => {
    confirmingDeleteName.value = null;
    actionError.value = '';
    await invalidateChannels();
  },
  onError: (err) => {
    actionError.value = extractApiError(err).message;
  },
});

const testMutation = useMutation({
  mutationFn: (name: string) => apiClient.testChannel(name),
  onSuccess: (result, name) => {
    probeByName.value = { ...probeByName.value, [name]: result };
    testingName.value = null;
  },
  onError: (err, name) => {
    testingName.value = null;
    actionError.value = `${name}: ${extractApiError(err).message}`;
  },
});

function openCreate() {
  editingName.value = null;
  editorName.value = '';
  editorProtocol.value = 'openai_chat';
  editorBaseUrl.value = '';
  editorApiKey.value = '';
  editorModels.value = '';
  editorAliases.value = '';
  editorPriority.value = '0';
  editorWeight.value = '1';
  editorTimeoutMs.value = '30000';
  editorMaxRetries.value = '0';
  editorError.value = '';
  clearErrors();
  showEditor.value = true;
}

function openEdit(channel: Channel) {
  editingName.value = channel.name;
  editorName.value = channel.name;
  editorProtocol.value = channel.protocol;
  editorBaseUrl.value = channel.base_url;
  editorApiKey.value = channel.api_key;
  editorModels.value = formatModelList(channel.models);
  editorAliases.value = formatAliases(channel.model_aliases);
  editorPriority.value = String(channel.priority);
  editorWeight.value = String(channel.weight);
  editorTimeoutMs.value = String(channel.timeout_ms);
  editorMaxRetries.value = String(channel.max_retries);
  editorError.value = '';
  clearErrors();
  showEditor.value = true;
}

function handleSave() {
  editorError.value = '';
  const specs: FieldValidationSpec[] = [
    { name: 'name', value: editorName.value, rules: [{ kind: 'required' }] },
    { name: 'baseUrl', value: editorBaseUrl.value, rules: [{ kind: 'required' }] },
    { name: 'apiKey', value: editorApiKey.value, rules: [{ kind: 'required' }] },
    {
      name: 'priority',
      value: editorPriority.value,
      rules: [{ kind: 'required' }, { kind: 'uint' }],
    },
    {
      name: 'weight',
      value: editorWeight.value,
      rules: [{ kind: 'required' }, { kind: 'uint', min: 1 }],
    },
    {
      name: 'timeoutMs',
      value: editorTimeoutMs.value,
      rules: [{ kind: 'required' }, { kind: 'uint', min: 1 }],
    },
    {
      name: 'maxRetries',
      value: editorMaxRetries.value,
      rules: [{ kind: 'required' }, { kind: 'uint' }],
    },
  ];
  if (!validate(specs, t)) return;
  const aliases = parseAliases(editorAliases.value);
  if (aliases === null) {
    editorError.value = t('channel.aliasesGuide');
    return;
  }
  const priority = parseOptionalUint(editorPriority.value);
  const weight = parseOptionalUint(editorWeight.value);
  const timeoutMs = parseOptionalUint(editorTimeoutMs.value);
  const maxRetries = parseOptionalUint(editorMaxRetries.value);
  if (priority === null || weight === null || timeoutMs === null || maxRetries === null) {
    return;
  }
  saveMutation.mutate({
    name: editorName.value.trim(),
    protocol: editorProtocol.value,
    base_url: editorBaseUrl.value.trim(),
    api_key: editorApiKey.value,
    models: parseModelList(editorModels.value),
    model_aliases: aliases,
    priority,
    weight,
    timeout_ms: timeoutMs,
    max_retries: maxRetries,
  });
}

function handleDelete(name: string) {
  if (confirmingDeleteName.value !== name) {
    confirmingDeleteName.value = name;
    return;
  }
  actionError.value = '';
  deleteMutation.mutate(name);
}

function handleTest(name: string) {
  actionError.value = '';
  testingName.value = name;
  testMutation.mutate(name);
}

function probeText(result: ChannelProbeResult): string {
  if (!result.reachable) {
    return t('channel.probeUnreachable', { latency: result.latency_ms });
  }
  if (result.error) {
    return t('channel.probeFailure', {
      status: result.status_code ?? '—',
      latency: result.latency_ms,
    });
  }
  return t('channel.probeSuccess', {
    status: result.status_code ?? '—',
    latency: result.latency_ms,
  });
}

function probeClass(result: ChannelProbeResult): string {
  return result.reachable && result.error === null ? 'badge-success' : 'badge-danger';
}
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col overflow-hidden">
    <PageHeader :title="t('nav.channel')" :subtitle="t('channel.subtitle')">
      <template #actions>
        <button
          type="button"
          class="btn btn-primary"
          data-testid="create-channel"
          @click="openCreate"
        >
          {{ t('channel.create') }}
        </button>
      </template>
    </PageHeader>

    <InlineError
      v-if="channelsQuery.isError.value && !channelsQuery.data.value"
      :message="extractApiError(channelsQuery.error.value).message"
      @retry="() => channelsQuery.refetch()"
    />

    <div v-else class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <p v-if="actionError" class="text-danger mb-4 shrink-0">{{ actionError }}</p>

      <DataTable fill-viewport class="min-h-0 flex-1" :busy="showTableSkeleton">
        <template #toolbar>
          <DataTableToolbar>
            <FormTextInput
              id="channels-search"
              v-model="searchText"
              type="text"
              class="h-8 max-w-xs"
              data-testid="channels-search"
              :placeholder="t('channel.search')"
              :aria-label="t('channel.search')"
            />
          </DataTableToolbar>
        </template>
        <TableHeader>
          <TableRow>
            <TableHead>{{ t('channel.name') }}</TableHead>
            <TableHead>{{ t('channel.protocol') }}</TableHead>
            <TableHead>{{ t('channel.baseUrl') }}</TableHead>
            <TableHead>{{ t('channel.models') }}</TableHead>
            <TableHead>{{ t('channel.priority') }}</TableHead>
            <TableHead>{{ t('channel.test') }}</TableHead>
            <TableHead align="center">{{ t('common.actions') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" :columns="7" />
          <template v-else>
            <TableRow
              v-for="channel in filteredChannels"
              :key="channel.name"
              data-testid="channel-row"
              :data-channel-name="channel.name"
            >
              <TableCell class="font-medium">{{ channel.name }}</TableCell>
              <TableCell>{{ t(`protocol.${channel.protocol}`) }}</TableCell>
              <TableCell class="font-mono text-sm">{{ channel.base_url }}</TableCell>
              <TableCell class="font-mono text-sm">
                {{ channel.models.join(', ') }}
              </TableCell>
              <TableCell class="font-mono">{{ channel.priority }}</TableCell>
              <TableCell>
                <span class="flex flex-col gap-1">
                  <button
                    type="button"
                    class="btn btn-sm btn-subtle"
                    data-testid="channel-test"
                    :disabled="testingName === channel.name"
                    @click="handleTest(channel.name)"
                  >
                    {{ testingName === channel.name ? t('channel.testing') : t('channel.test') }}
                  </button>
                  <span
                    v-if="probeByName[channel.name]"
                    class="badge"
                    :class="probeClass(probeByName[channel.name]!)"
                    data-testid="channel-probe-result"
                  >
                    {{ probeText(probeByName[channel.name]!) }}
                  </span>
                </span>
              </TableCell>
              <TableCell align="center">
                <span
                  v-if="confirmingDeleteName === channel.name"
                  class="inline-flex items-center justify-center gap-1"
                >
                  <button
                    type="button"
                    class="btn btn-sm btn-danger-filled"
                    data-testid="channel-delete-confirm"
                    @click="handleDelete(channel.name)"
                  >
                    {{ t('common.confirmDelete') }}
                  </button>
                  <button
                    type="button"
                    class="btn btn-sm btn-ghost"
                    @click="confirmingDeleteName = null"
                  >
                    {{ t('common.cancel') }}
                  </button>
                </span>
                <DataTableRowActions v-else>
                  <DataTableMenuItem data-testid="channel-edit" @select="openEdit(channel)">
                    {{ t('common.edit') }}
                  </DataTableMenuItem>
                  <DataTableMenuSeparator />
                  <DataTableMenuItem
                    danger
                    data-testid="channel-delete"
                    @select="handleDelete(channel.name)"
                  >
                    {{ t('common.delete') }}
                  </DataTableMenuItem>
                </DataTableRowActions>
              </TableCell>
            </TableRow>
            <TableRow v-if="filteredChannels.length === 0">
              <TableCell :colspan="7" class="h-24 whitespace-normal">
                <EmptyState :title="t('common.emptyList')">
                  <button type="button" class="btn btn-primary" @click="openCreate">
                    {{ t('channel.create') }}
                  </button>
                </EmptyState>
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
    </div>

    <AppModal v-if="showEditor" wide :labelled-by="editorTitleId" @close="showEditor = false">
      <div class="card w-full">
        <form novalidate data-testid="channel-form" @submit.prevent="handleSave">
          <div class="card-header">
            <h2 :id="editorTitleId" class="font-serif text-base font-semibold">
              {{ editingName === null ? t('channel.editorCreate') : t('channel.editorEdit') }}
            </h2>
          </div>
          <div class="card-body space-y-3">
            <FormField
              field-name="name"
              :label="t('channel.name')"
              input-id="channel-editor-name"
              :error="fieldError('name')"
            >
              <template #default="{ hintId, invalid }">
                <FormTextInput
                  id="channel-editor-name"
                  v-model="editorName"
                  type="text"
                  :disabled="editingName !== null"
                  :invalid="invalid"
                  :hint-id="hintId"
                  v-on="fieldInputHandlers('name')"
                />
              </template>
            </FormField>
            <FormField
              field-name="protocol"
              :label="t('channel.protocol')"
              input-id="channel-editor-protocol"
            >
              <template #default>
                <UiSelect
                  id="channel-editor-protocol"
                  v-model="editorProtocol"
                  :options="protocolOptions"
                />
              </template>
            </FormField>
            <FormField
              field-name="baseUrl"
              :label="t('channel.baseUrl')"
              input-id="channel-editor-base-url"
              :error="fieldError('baseUrl')"
            >
              <template #default="{ hintId, invalid }">
                <FormTextInput
                  id="channel-editor-base-url"
                  v-model="editorBaseUrl"
                  type="url"
                  :invalid="invalid"
                  :hint-id="hintId"
                  v-on="fieldInputHandlers('baseUrl')"
                />
              </template>
            </FormField>
            <FormField
              field-name="apiKey"
              :label="t('channel.apiKey')"
              input-id="channel-editor-api-key"
              :error="fieldError('apiKey')"
            >
              <template #default="{ hintId, invalid }">
                <FormPasswordInput
                  id="channel-editor-api-key"
                  v-model="editorApiKey"
                  autocomplete="off"
                  :invalid="invalid"
                  :hint-id="hintId"
                  v-on="fieldInputHandlers('apiKey')"
                />
              </template>
            </FormField>
            <FormField
              field-name="models"
              :label="t('channel.models')"
              input-id="channel-editor-models"
              :guide="t('channel.modelsGuide')"
            >
              <template #default="{ hintId, invalid }">
                <FormTextarea
                  id="channel-editor-models"
                  v-model="editorModels"
                  rows="3"
                  :invalid="invalid"
                  :hint-id="hintId"
                />
              </template>
            </FormField>
            <FormField
              field-name="aliases"
              :label="t('channel.aliases')"
              input-id="channel-editor-aliases"
              :guide="t('channel.aliasesGuide')"
            >
              <template #default>
                <FormTextarea id="channel-editor-aliases" v-model="editorAliases" rows="3" />
              </template>
            </FormField>
            <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <FormField
                field-name="priority"
                :label="t('channel.priority')"
                input-id="channel-editor-priority"
                :error="fieldError('priority')"
              >
                <template #default="{ hintId, invalid }">
                  <FormTextInput
                    id="channel-editor-priority"
                    v-model="editorPriority"
                    type="text"
                    inputmode="numeric"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('priority')"
                  />
                </template>
              </FormField>
              <FormField
                field-name="weight"
                :label="t('channel.weight')"
                input-id="channel-editor-weight"
                :error="fieldError('weight')"
              >
                <template #default="{ hintId, invalid }">
                  <FormTextInput
                    id="channel-editor-weight"
                    v-model="editorWeight"
                    type="text"
                    inputmode="numeric"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('weight')"
                  />
                </template>
              </FormField>
              <FormField
                field-name="timeoutMs"
                :label="t('channel.timeoutMs')"
                input-id="channel-editor-timeout-ms"
                :error="fieldError('timeoutMs')"
              >
                <template #default="{ hintId, invalid }">
                  <FormTextInput
                    id="channel-editor-timeout-ms"
                    v-model="editorTimeoutMs"
                    type="text"
                    inputmode="numeric"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('timeoutMs')"
                  />
                </template>
              </FormField>
              <FormField
                field-name="maxRetries"
                :label="t('channel.maxRetries')"
                input-id="channel-editor-max-retries"
                :error="fieldError('maxRetries')"
              >
                <template #default="{ hintId, invalid }">
                  <FormTextInput
                    id="channel-editor-max-retries"
                    v-model="editorMaxRetries"
                    type="text"
                    inputmode="numeric"
                    :invalid="invalid"
                    :hint-id="hintId"
                    v-on="fieldInputHandlers('maxRetries')"
                  />
                </template>
              </FormField>
            </div>
            <p v-if="editorError" class="text-danger text-sm" data-testid="channel-editor-error">
              {{ editorError }}
            </p>
          </div>
          <div class="card-footer card-body flex justify-end gap-2">
            <button type="button" class="btn" @click="showEditor = false">
              {{ t('common.cancel') }}
            </button>
            <button
              type="submit"
              class="btn btn-primary"
              data-testid="channel-save"
              :disabled="saveMutation.isPending.value"
            >
              {{ t('common.save') }}
            </button>
          </div>
        </form>
      </div>
    </AppModal>
  </div>
</template>
