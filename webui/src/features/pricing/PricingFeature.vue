<script setup lang="ts">
import { computed, ref, useId } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { Price } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import AppModal from '@/components/ui/AppModal.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FormField from '@/components/ui/FormField.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import InlineError from '@/components/ui/InlineError.vue';
import DataTable from '@/components/ui/data-table/DataTable.vue';
import DataTableMenuItem from '@/components/ui/data-table/DataTableMenuItem.vue';
import DataTableMenuSeparator from '@/components/ui/data-table/DataTableMenuSeparator.vue';
import DataTableRowActions from '@/components/ui/data-table/DataTableRowActions.vue';
import DataTableToolbar from '@/components/ui/data-table/DataTableToolbar.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { formatUsdAmount, formatUsdMicros, parseUsdToMicros } from '@/lib/format';
import type { FieldValidationSpec } from '@/lib/form-validation';

const { t } = useI18n();
const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, clearErrors, validate } = useFormValidation();

const searchText = ref('');
const showEditor = ref(false);
const editingModel = ref<string | null>(null);
const editorModel = ref('');
const editorInput = ref('');
const editorOutput = ref('');
const editorCacheRead = ref('');
const editorCacheWrite = ref('');
const editorError = ref('');
const editorTitleId = useId();
const confirmingDeleteModel = ref<string | null>(null);
const actionError = ref('');

const pricesQuery = useQuery({
  queryKey: ['prices'],
  queryFn: () => apiClient.listPrices(),
});

const prices = computed(() => pricesQuery.data.value ?? []);
const showTableSkeleton = computed(() => pricesQuery.isPending.value && !pricesQuery.data.value);

const filteredPrices = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  if (!q) return prices.value;
  return prices.value.filter((price) => price.model.toLowerCase().includes(q));
});

function invalidatePrices() {
  return queryClient.invalidateQueries({ queryKey: ['prices'] });
}

const saveMutation = useMutation({
  mutationFn: (body: Price) =>
    editingModel.value === null
      ? apiClient.createPrice(body)
      : apiClient.updatePrice(editingModel.value, body),
  onSuccess: async () => {
    editorError.value = '';
    showEditor.value = false;
    await invalidatePrices();
  },
  onError: (err) => {
    editorError.value = extractApiError(err).message;
  },
});

const deleteMutation = useMutation({
  mutationFn: (model: string) => apiClient.deletePrice(model),
  onSuccess: async () => {
    confirmingDeleteModel.value = null;
    actionError.value = '';
    await invalidatePrices();
  },
  onError: (err) => {
    actionError.value = extractApiError(err).message;
  },
});

function openCreate() {
  editingModel.value = null;
  editorModel.value = '';
  editorInput.value = '';
  editorOutput.value = '';
  editorCacheRead.value = '';
  editorCacheWrite.value = '';
  editorError.value = '';
  clearErrors();
  showEditor.value = true;
}

function openEdit(price: Price) {
  editingModel.value = price.model;
  editorModel.value = price.model;
  editorInput.value = formatUsdAmount(price.input_micros);
  editorOutput.value = formatUsdAmount(price.output_micros);
  editorCacheRead.value =
    price.cache_read_micros === null ? '' : formatUsdAmount(price.cache_read_micros);
  editorCacheWrite.value =
    price.cache_write_micros === null ? '' : formatUsdAmount(price.cache_write_micros);
  editorError.value = '';
  clearErrors();
  showEditor.value = true;
}

function optionalMicros(value: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  return parseUsdToMicros(trimmed);
}

function handleSave() {
  editorError.value = '';
  const specs: FieldValidationSpec[] = [
    { name: 'model', value: editorModel.value, rules: [{ kind: 'required' }] },
    {
      name: 'input',
      value: editorInput.value,
      rules: [{ kind: 'required' }, { kind: 'usd', min: 0 }],
    },
    {
      name: 'output',
      value: editorOutput.value,
      rules: [{ kind: 'required' }, { kind: 'usd', min: 0 }],
    },
    { name: 'cacheRead', value: editorCacheRead.value, rules: [{ kind: 'usd', min: 0 }] },
    { name: 'cacheWrite', value: editorCacheWrite.value, rules: [{ kind: 'usd', min: 0 }] },
  ];
  if (!validate(specs, t)) return;
  const inputMicros = parseUsdToMicros(editorInput.value);
  const outputMicros = parseUsdToMicros(editorOutput.value);
  if (inputMicros === null || outputMicros === null) {
    return;
  }
  saveMutation.mutate({
    model: editorModel.value.trim(),
    input_micros: inputMicros,
    output_micros: outputMicros,
    cache_read_micros: optionalMicros(editorCacheRead.value),
    cache_write_micros: optionalMicros(editorCacheWrite.value),
  });
}

function handleDelete(model: string) {
  if (confirmingDeleteModel.value !== model) {
    confirmingDeleteModel.value = model;
    return;
  }
  actionError.value = '';
  deleteMutation.mutate(model);
}

function formatOptionalMicros(value: number | null): string {
  return value === null ? '—' : formatUsdMicros(value);
}
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col overflow-hidden">
    <PageHeader :title="t('nav.pricing')" :subtitle="t('pricing.subtitle')">
      <template #actions>
        <button
          type="button"
          class="btn btn-primary"
          data-testid="pricing-create-entry"
          @click="openCreate"
        >
          {{ t('pricing.createEntry') }}
        </button>
      </template>
    </PageHeader>

    <InlineError
      v-if="pricesQuery.isError.value && !pricesQuery.data.value"
      :message="extractApiError(pricesQuery.error.value).message"
      @retry="() => pricesQuery.refetch()"
    />

    <div v-else class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <p v-if="actionError" class="text-danger mb-4 shrink-0">{{ actionError }}</p>

      <DataTable fill-viewport class="min-h-0 flex-1" :busy="showTableSkeleton">
        <template #toolbar>
          <DataTableToolbar>
            <FormTextInput
              id="pricing-search"
              v-model="searchText"
              type="text"
              class="h-8 max-w-xs"
              data-testid="pricing-search"
              :placeholder="t('pricing.model')"
              :aria-label="t('pricing.model')"
            />
          </DataTableToolbar>
        </template>
        <TableHeader>
          <TableRow>
            <TableHead>{{ t('pricing.model') }}</TableHead>
            <TableHead>{{ t('pricing.input') }}</TableHead>
            <TableHead>{{ t('pricing.output') }}</TableHead>
            <TableHead>{{ t('pricing.cacheRead') }}</TableHead>
            <TableHead>{{ t('pricing.cacheWrite') }}</TableHead>
            <TableHead align="center">{{ t('common.actions') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" :columns="6" />
          <template v-else>
            <TableRow
              v-for="price in filteredPrices"
              :key="price.model"
              data-testid="price-row"
              :data-price-model="price.model"
            >
              <TableCell class="font-medium">{{ price.model }}</TableCell>
              <TableCell class="font-mono" data-testid="price-input">
                {{ formatUsdMicros(price.input_micros) }}
              </TableCell>
              <TableCell class="font-mono" data-testid="price-output">
                {{ formatUsdMicros(price.output_micros) }}
              </TableCell>
              <TableCell class="font-mono" data-testid="price-cache-read">
                {{ formatOptionalMicros(price.cache_read_micros) }}
              </TableCell>
              <TableCell class="font-mono" data-testid="price-cache-write">
                {{ formatOptionalMicros(price.cache_write_micros) }}
              </TableCell>
              <TableCell align="center">
                <span
                  v-if="confirmingDeleteModel === price.model"
                  class="inline-flex items-center justify-center gap-1"
                >
                  <button
                    type="button"
                    class="btn btn-sm btn-danger-filled"
                    data-testid="pricing-delete-confirm"
                    @click="handleDelete(price.model)"
                  >
                    {{ t('common.confirmDelete') }}
                  </button>
                  <button
                    type="button"
                    class="btn btn-sm btn-ghost"
                    @click="confirmingDeleteModel = null"
                  >
                    {{ t('common.cancel') }}
                  </button>
                </span>
                <DataTableRowActions v-else>
                  <DataTableMenuItem data-testid="pricing-edit-entry" @select="openEdit(price)">
                    {{ t('common.edit') }}
                  </DataTableMenuItem>
                  <DataTableMenuSeparator />
                  <DataTableMenuItem
                    danger
                    data-testid="pricing-delete-entry"
                    @select="handleDelete(price.model)"
                  >
                    {{ t('common.delete') }}
                  </DataTableMenuItem>
                </DataTableRowActions>
              </TableCell>
            </TableRow>
            <TableRow v-if="filteredPrices.length === 0">
              <TableCell :colspan="6" class="h-24 whitespace-normal">
                <EmptyState :title="t('common.emptyList')">
                  <button type="button" class="btn btn-primary" @click="openCreate">
                    {{ t('pricing.createEntry') }}
                  </button>
                </EmptyState>
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
    </div>

    <AppModal v-if="showEditor" :labelled-by="editorTitleId" @close="showEditor = false">
      <div class="card w-full">
        <form novalidate @submit.prevent="handleSave">
          <div class="card-header">
            <h2 :id="editorTitleId" class="font-serif text-base font-semibold">
              {{ editingModel === null ? t('pricing.editorCreate') : t('pricing.editorEdit') }}
            </h2>
          </div>
          <div class="card-body space-y-3">
            <FormField
              field-name="model"
              :label="t('pricing.model')"
              input-id="pricing-editor-model"
              :error="fieldError('model')"
            >
              <template #default="{ hintId, invalid }">
                <FormTextInput
                  id="pricing-editor-model"
                  v-model="editorModel"
                  type="text"
                  :disabled="editingModel !== null"
                  :invalid="invalid"
                  :hint-id="hintId"
                  v-on="fieldInputHandlers('model')"
                />
              </template>
            </FormField>
            <FormField
              field-name="input"
              :label="t('pricing.input')"
              input-id="pricing-editor-input"
              :error="fieldError('input')"
              :guide="t('pricing.usdGuide')"
            >
              <template #default="{ hintId, invalid }">
                <FormTextInput
                  id="pricing-editor-input"
                  v-model="editorInput"
                  type="text"
                  inputmode="decimal"
                  class="font-mono"
                  :invalid="invalid"
                  :hint-id="hintId"
                  v-on="fieldInputHandlers('input')"
                />
              </template>
            </FormField>
            <FormField
              field-name="output"
              :label="t('pricing.output')"
              input-id="pricing-editor-output"
              :error="fieldError('output')"
              :guide="t('pricing.usdGuide')"
            >
              <template #default="{ hintId, invalid }">
                <FormTextInput
                  id="pricing-editor-output"
                  v-model="editorOutput"
                  type="text"
                  inputmode="decimal"
                  class="font-mono"
                  :invalid="invalid"
                  :hint-id="hintId"
                  v-on="fieldInputHandlers('output')"
                />
              </template>
            </FormField>
            <FormField
              field-name="cacheRead"
              :label="t('pricing.cacheRead')"
              input-id="pricing-editor-cache-read"
              :error="fieldError('cacheRead')"
              :guide="t('pricing.optionalUsdGuide')"
            >
              <template #default="{ hintId, invalid }">
                <FormTextInput
                  id="pricing-editor-cache-read"
                  v-model="editorCacheRead"
                  type="text"
                  inputmode="decimal"
                  class="font-mono"
                  :invalid="invalid"
                  :hint-id="hintId"
                  v-on="fieldInputHandlers('cacheRead')"
                />
              </template>
            </FormField>
            <FormField
              field-name="cacheWrite"
              :label="t('pricing.cacheWrite')"
              input-id="pricing-editor-cache-write"
              :error="fieldError('cacheWrite')"
              :guide="t('pricing.optionalUsdGuide')"
            >
              <template #default="{ hintId, invalid }">
                <FormTextInput
                  id="pricing-editor-cache-write"
                  v-model="editorCacheWrite"
                  type="text"
                  inputmode="decimal"
                  class="font-mono"
                  :invalid="invalid"
                  :hint-id="hintId"
                  v-on="fieldInputHandlers('cacheWrite')"
                />
              </template>
            </FormField>
            <p v-if="editorError" class="text-danger text-sm" data-testid="pricing-editor-error">
              {{ editorError }}
            </p>
          </div>
          <div class="card-footer card-body flex justify-end gap-2">
            <button type="button" class="btn" @click="showEditor = false">
              {{ t('common.close') }}
            </button>
            <button
              type="submit"
              class="btn btn-primary"
              data-testid="pricing-save-entry"
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
