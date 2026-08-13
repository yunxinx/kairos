<script setup lang="ts">
import { computed, ref, useId } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { BalanceView, Token } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import AppModal from '@/components/ui/AppModal.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FormField from '@/components/ui/FormField.vue';
import FormTextInput from '@/components/ui/FormTextInput.vue';
import InlineError from '@/components/ui/InlineError.vue';
import DataTablePanel from '@/components/ui/DataTablePanel.vue';
import TableSkeleton from '@/components/ui/TableSkeleton.vue';
import VirtualDataTable from '@/components/ui/VirtualDataTable.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import { useFormValidation } from '@/composables/useFormValidation';
import { formatUsdAmount, formatUsdMicros, parseUsdToMicros } from '@/lib/format';
import { managementTableColumnPresets } from '@/lib/management-table-column-presets';
import type { FieldValidationSpec } from '@/lib/form-validation';

type TokenRow = Token & {
  balance_usd_micros: number;
  settled_usd_micros: number;
};

type BalanceMode = 'recharge' | 'deduct';

const { t } = useI18n();
const queryClient = useQueryClient();
const { fieldError, fieldInputHandlers, clearErrors, validate } = useFormValidation();

const searchText = ref('');
const showEditor = ref(false);
const editingKey = ref<string | null>(null);
const editorKey = ref('');
const editorName = ref('');
const editorLimit = ref('');
const editorError = ref('');
const editorTitleId = useId();

const confirmingDeleteKey = ref<string | null>(null);
const actionError = ref('');

const showBalance = ref(false);
const balanceMode = ref<BalanceMode>('recharge');
const balanceTokenKey = ref('');
const balanceAmount = ref('');
const balanceError = ref('');
const balanceTitleId = useId();

const tokensQuery = useQuery({
  queryKey: ['tokens'],
  queryFn: loadTokenRows,
});

const tokens = computed(() => tokensQuery.data.value ?? []);

const filteredTokens = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  if (!q) return tokens.value;
  return tokens.value.filter(
    (token) => token.name.toLowerCase().includes(q) || token.token_key.toLowerCase().includes(q),
  );
});

async function loadTokenRows(): Promise<TokenRow[]> {
  const listed = await apiClient.listTokens();
  // 余额读是 `POST .../balance` 且 delta=0，仍占 SQLite 写锁；必须串行，避免 Promise.all 并发写触发 database is locked。
  const balances: BalanceView[] = [];
  for (const token of listed) {
    balances.push(await apiClient.readTokenBalance(token.token_key));
  }
  const byKey = new Map(balances.map((item) => [item.token_key, item]));
  return listed.map((token) => {
    const balance = byKey.get(token.token_key);
    return {
      ...token,
      balance_usd_micros: balance?.balance_usd_micros ?? 0,
      settled_usd_micros: balance?.settled_usd_micros ?? 0,
    };
  });
}

function invalidateTokens() {
  return queryClient.invalidateQueries({ queryKey: ['tokens'] });
}

const saveMutation = useMutation({
  mutationFn: (body: Token) =>
    editingKey.value === null
      ? apiClient.createToken(body)
      : apiClient.updateToken(editingKey.value, body),
  onSuccess: async () => {
    editorError.value = '';
    showEditor.value = false;
    await invalidateTokens();
  },
  onError: (err) => {
    editorError.value = extractApiError(err).message;
  },
});

const deleteMutation = useMutation({
  mutationFn: (tokenKey: string) => apiClient.deleteToken(tokenKey),
  onSuccess: async () => {
    confirmingDeleteKey.value = null;
    actionError.value = '';
    await invalidateTokens();
  },
  onError: (err) => {
    actionError.value = extractApiError(err).message;
  },
});

const balanceMutation = useMutation({
  mutationFn: ({ tokenKey, delta }: { tokenKey: string; delta: number }) =>
    apiClient.adjustTokenBalance(tokenKey, { delta_usd_micros: delta }),
  onSuccess: async () => {
    balanceError.value = '';
    showBalance.value = false;
    await invalidateTokens();
  },
  onError: (err) => {
    balanceError.value = extractApiError(err).message;
  },
});

function openCreate() {
  editingKey.value = null;
  editorKey.value = '';
  editorName.value = '';
  editorLimit.value = '';
  editorError.value = '';
  clearErrors();
  showEditor.value = true;
}

function openEdit(token: TokenRow) {
  editingKey.value = token.token_key;
  editorKey.value = token.token_key;
  editorName.value = token.name;
  editorLimit.value =
    token.limit_usd_micros === null ? '' : formatUsdAmount(token.limit_usd_micros);
  editorError.value = '';
  clearErrors();
  showEditor.value = true;
}

function openBalance(token: TokenRow, mode: BalanceMode) {
  balanceMode.value = mode;
  balanceTokenKey.value = token.token_key;
  balanceAmount.value = '';
  balanceError.value = '';
  clearErrors();
  showBalance.value = true;
}

function handleSave() {
  editorError.value = '';
  const specs: FieldValidationSpec[] = [
    { name: 'tokenKey', value: editorKey.value, rules: [{ kind: 'required' }] },
    { name: 'name', value: editorName.value, rules: [{ kind: 'required' }] },
    { name: 'limit', value: editorLimit.value, rules: [{ kind: 'usd', min: 0 }] },
  ];
  if (!validate(specs, t)) return;
  const limit = parseUsdToMicros(editorLimit.value);
  saveMutation.mutate({
    token_key: editorKey.value.trim(),
    name: editorName.value.trim(),
    limit_usd_micros: limit,
  });
}

function handleBalance() {
  balanceError.value = '';
  if (
    !validate(
      [
        {
          name: 'amount',
          value: balanceAmount.value,
          rules: [{ kind: 'required' }, { kind: 'usd', min: 0 }],
        },
      ],
      t,
    )
  ) {
    return;
  }
  const micros = parseUsdToMicros(balanceAmount.value);
  if (micros === null || micros === 0) {
    balanceError.value = t('validation.usd');
    return;
  }
  const delta = balanceMode.value === 'deduct' ? -micros : micros;
  balanceMutation.mutate({ tokenKey: balanceTokenKey.value, delta });
}

function handleDelete(tokenKey: string) {
  if (confirmingDeleteKey.value !== tokenKey) {
    confirmingDeleteKey.value = tokenKey;
    return;
  }
  actionError.value = '';
  deleteMutation.mutate(tokenKey);
}

function formatLimit(limit: number | null): string {
  return limit === null ? t('common.unlimited') : formatUsdMicros(limit);
}
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col overflow-hidden">
    <PageHeader :title="t('nav.tokens')" :subtitle="t('tokens.subtitle')">
      <template #actions>
        <button
          type="button"
          class="btn btn-primary"
          data-testid="create-token"
          @click="openCreate"
        >
          {{ t('tokens.create') }}
        </button>
      </template>
    </PageHeader>

    <div class="card mb-4 shrink-0">
      <div class="card-body">
        <FormTextInput
          id="tokens-search"
          v-model="searchText"
          type="text"
          class="max-w-md"
          data-testid="tokens-search"
          :placeholder="t('tokens.search')"
          :aria-label="t('tokens.search')"
        />
      </div>
    </div>

    <TableSkeleton
      v-if="tokensQuery.isPending.value"
      fill-viewport
      class="min-h-0 flex-1"
      :columns="6"
    />

    <InlineError
      v-else-if="tokensQuery.isError.value"
      :message="extractApiError(tokensQuery.error.value).message"
      @retry="() => tokensQuery.refetch()"
    />

    <div v-else class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <p v-if="actionError" class="text-danger mb-4 shrink-0">{{ actionError }}</p>

      <DataTablePanel fill-viewport class="min-h-0 flex-1">
        <VirtualDataTable
          :row-count="filteredTokens.length"
          :columns="managementTableColumnPresets.tokens"
          :estimate-row-height="56"
        >
          <template #header>
            <TableHeader>
              <TableRow>
                <TableHead>{{ t('tokens.name') }}</TableHead>
                <TableHead>{{ t('tokens.key') }}</TableHead>
                <TableHead>{{ t('tokens.balance') }}</TableHead>
                <TableHead>{{ t('tokens.settled') }}</TableHead>
                <TableHead>{{ t('tokens.limit') }}</TableHead>
                <TableHead>{{ t('common.actions') }}</TableHead>
              </TableRow>
            </TableHeader>
          </template>
          <template #row="{ index }">
            <TableRow
              v-if="filteredTokens[index]"
              :key="filteredTokens[index].token_key"
              data-testid="token-row"
              :data-token-key="filteredTokens[index].token_key"
            >
              <TableCell class="font-medium">{{ filteredTokens[index].name }}</TableCell>
              <TableCell>
                <code class="code-chip rounded px-2 py-0.5 font-mono text-xs">
                  {{ filteredTokens[index].token_key }}
                </code>
              </TableCell>
              <TableCell class="font-mono" data-testid="token-balance">
                {{ formatUsdMicros(filteredTokens[index].balance_usd_micros) }}
              </TableCell>
              <TableCell class="font-mono" data-testid="token-settled">
                {{ formatUsdMicros(filteredTokens[index].settled_usd_micros) }}
              </TableCell>
              <TableCell class="font-mono">
                {{ formatLimit(filteredTokens[index].limit_usd_micros) }}
              </TableCell>
              <TableCell>
                <span class="inline-flex flex-wrap items-center gap-1">
                  <button
                    type="button"
                    class="btn btn-sm btn-subtle"
                    data-testid="token-recharge"
                    @click="openBalance(filteredTokens[index], 'recharge')"
                  >
                    {{ t('tokens.recharge') }}
                  </button>
                  <button
                    type="button"
                    class="btn btn-sm btn-subtle"
                    data-testid="token-deduct"
                    @click="openBalance(filteredTokens[index], 'deduct')"
                  >
                    {{ t('tokens.deduct') }}
                  </button>
                  <button
                    type="button"
                    class="btn btn-sm btn-subtle"
                    data-testid="token-edit"
                    @click="openEdit(filteredTokens[index])"
                  >
                    {{ t('common.edit') }}
                  </button>
                  <button
                    v-if="confirmingDeleteKey !== filteredTokens[index].token_key"
                    type="button"
                    class="btn btn-sm btn-subtle text-danger"
                    data-testid="token-delete"
                    @click="handleDelete(filteredTokens[index].token_key)"
                  >
                    {{ t('common.delete') }}
                  </button>
                  <template v-else>
                    <button
                      type="button"
                      class="btn btn-sm btn-danger-filled"
                      data-testid="token-delete-confirm"
                      @click="handleDelete(filteredTokens[index].token_key)"
                    >
                      {{ t('common.confirmDelete') }}
                    </button>
                    <button
                      type="button"
                      class="btn btn-sm btn-ghost"
                      @click="confirmingDeleteKey = null"
                    >
                      {{ t('common.cancel') }}
                    </button>
                  </template>
                </span>
              </TableCell>
            </TableRow>
          </template>
          <template v-if="filteredTokens.length === 0" #empty>
            <EmptyState :title="t('common.emptyList')">
              <button type="button" class="btn btn-primary" @click="openCreate">
                {{ t('tokens.create') }}
              </button>
            </EmptyState>
          </template>
        </VirtualDataTable>
      </DataTablePanel>
    </div>

    <AppModal v-if="showEditor" :labelled-by="editorTitleId" @close="showEditor = false">
      <div class="card w-full">
        <form novalidate @submit.prevent="handleSave">
          <div class="card-header">
            <h2 :id="editorTitleId" class="font-serif text-base font-semibold">
              {{ editingKey === null ? t('tokens.editorCreate') : t('tokens.editorEdit') }}
            </h2>
          </div>
          <div class="card-body space-y-3">
            <FormField
              field-name="tokenKey"
              :label="t('tokens.key')"
              input-id="token-editor-key"
              :error="fieldError('tokenKey')"
            >
              <template #default="{ hintId, invalid }">
                <FormTextInput
                  id="token-editor-key"
                  v-model="editorKey"
                  type="text"
                  class="font-mono"
                  :disabled="editingKey !== null"
                  :invalid="invalid"
                  :hint-id="hintId"
                  v-on="fieldInputHandlers('tokenKey')"
                />
              </template>
            </FormField>
            <FormField
              field-name="name"
              :label="t('tokens.name')"
              input-id="token-editor-name"
              :error="fieldError('name')"
            >
              <template #default="{ hintId, invalid }">
                <FormTextInput
                  id="token-editor-name"
                  v-model="editorName"
                  type="text"
                  :placeholder="t('tokens.namePlaceholder')"
                  :invalid="invalid"
                  :hint-id="hintId"
                  v-on="fieldInputHandlers('name')"
                />
              </template>
            </FormField>
            <FormField
              field-name="limit"
              :label="t('tokens.limit')"
              input-id="token-editor-limit"
              :error="fieldError('limit')"
              :guide="t('tokens.limitGuide')"
            >
              <template #default="{ hintId, invalid }">
                <FormTextInput
                  id="token-editor-limit"
                  v-model="editorLimit"
                  type="text"
                  inputmode="decimal"
                  class="font-mono"
                  :invalid="invalid"
                  :hint-id="hintId"
                  v-on="fieldInputHandlers('limit')"
                />
              </template>
            </FormField>
            <p v-if="editorError" class="text-danger text-sm" data-testid="token-editor-error">
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
              data-testid="token-save"
              :disabled="saveMutation.isPending.value"
            >
              {{ t('common.save') }}
            </button>
          </div>
        </form>
      </div>
    </AppModal>

    <AppModal v-if="showBalance" :labelled-by="balanceTitleId" @close="showBalance = false">
      <div class="card w-full">
        <form novalidate @submit.prevent="handleBalance">
          <div class="card-header">
            <h2 :id="balanceTitleId" class="font-serif text-base font-semibold">
              {{ balanceMode === 'recharge' ? t('tokens.rechargeTitle') : t('tokens.deductTitle') }}
            </h2>
          </div>
          <div class="card-body space-y-3">
            <FormField
              field-name="amount"
              :label="t('tokens.amount')"
              input-id="token-balance-amount"
              :error="fieldError('amount')"
              :guide="t('tokens.amountGuide')"
            >
              <template #default="{ hintId, invalid }">
                <FormTextInput
                  id="token-balance-amount"
                  v-model="balanceAmount"
                  type="text"
                  inputmode="decimal"
                  class="font-mono"
                  :invalid="invalid"
                  :hint-id="hintId"
                  v-on="fieldInputHandlers('amount')"
                />
              </template>
            </FormField>
            <p v-if="balanceError" class="text-danger text-sm" data-testid="token-balance-error">
              {{ balanceError }}
            </p>
          </div>
          <div class="card-footer card-body flex justify-end gap-2">
            <button type="button" class="btn" @click="showBalance = false">
              {{ t('common.cancel') }}
            </button>
            <button
              type="submit"
              class="btn btn-primary"
              data-testid="token-balance-save"
              :disabled="balanceMutation.isPending.value"
            >
              {{ t('common.save') }}
            </button>
          </div>
        </form>
      </div>
    </AppModal>
  </div>
</template>
