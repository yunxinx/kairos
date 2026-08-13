<script setup lang="ts">
import { computed, ref } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import { loadTokenRows, type TokenRow } from '@/api/token-rows';
import PageHeader from '@/app/layout/PageHeader.vue';
import ConfirmWindow from '@/components/ui/ConfirmWindow.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
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
import { useWindowStack } from '@/composables/useWindowStack';
import type { BalanceMode } from '@/features/tokens/balance-mode';
import TokenBalanceWindow from '@/features/tokens/TokenBalanceWindow.vue';
import TokenEditorWindow from '@/features/tokens/TokenEditorWindow.vue';
import { formatUsdMicros } from '@/lib/format';
import { anchorFromEvent, type FloatingWindowAnchor } from '@/lib/window-anchor';

type TokenWindowPayload =
  | { kind: 'editor'; token: TokenRow | null }
  | { kind: 'balance'; token: TokenRow; mode: BalanceMode }
  | { kind: 'delete'; token: TokenRow };

const { t } = useI18n();
const queryClient = useQueryClient();

const searchText = ref('');
const pendingAnchor = ref<FloatingWindowAnchor | null>(null);

function takePendingAnchor(): FloatingWindowAnchor | null {
  const anchor = pendingAnchor.value;
  pendingAnchor.value = null;
  return anchor;
}

const {
  windows,
  topmostId,
  open: openWindow,
  close: closeWindow,
  setDirty,
  bringToFront,
} = useWindowStack<TokenWindowPayload>();

const tokensQuery = useQuery({
  queryKey: ['tokens'],
  queryFn: loadTokenRows,
});

const tokens = computed(() => tokensQuery.data.value ?? []);
const showTableSkeleton = computed(() => tokensQuery.isPending.value && !tokensQuery.data.value);

const filteredTokens = computed(() => {
  const q = searchText.value.trim().toLowerCase();
  if (!q) return tokens.value;
  return tokens.value.filter(
    (token) => token.name.toLowerCase().includes(q) || token.token_key.toLowerCase().includes(q),
  );
});

const deleteErrors = ref<Record<number, string>>({});

const deleteMutation = useMutation({
  mutationFn: (tokenKey: string) => apiClient.deleteToken(tokenKey),
  onSuccess: async (_data, tokenKey) => {
    const entry = windows.value.find(
      (item) => item.payload.kind === 'delete' && item.payload.token.token_key === tokenKey,
    );
    if (entry) closeWindow(entry.id);
    await queryClient.invalidateQueries({ queryKey: ['tokens'] });
  },
  onError: (err, tokenKey) => {
    const entry = windows.value.find(
      (item) => item.payload.kind === 'delete' && item.payload.token.token_key === tokenKey,
    );
    if (entry) deleteErrors.value[entry.id] = extractApiError(err).message;
  },
});

const deletingKey = computed(() =>
  deleteMutation.isPending.value ? (deleteMutation.variables.value ?? null) : null,
);

function openCreate(event: Event) {
  openWindow(anchorFromEvent(event), { kind: 'editor', token: null });
}

function openEdit(token: TokenRow) {
  const existing = windows.value.find(
    (entry) =>
      entry.payload.kind === 'editor' && entry.payload.token?.token_key === token.token_key,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'editor', token });
}

function openBalance(token: TokenRow, mode: BalanceMode) {
  const existing = windows.value.find(
    (entry) =>
      entry.payload.kind === 'balance' &&
      entry.payload.token.token_key === token.token_key &&
      entry.payload.mode === mode,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  openWindow(takePendingAnchor(), { kind: 'balance', token, mode });
}

function openDelete(token: TokenRow) {
  const existing = windows.value.find(
    (entry) => entry.payload.kind === 'delete' && entry.payload.token.token_key === token.token_key,
  );
  if (existing) {
    bringToFront(existing.id);
    return;
  }
  const entry = openWindow(takePendingAnchor(), { kind: 'delete', token });
  if (entry) deleteErrors.value[entry.id] = '';
}

function formatLimit(limit: number | null): string {
  return limit === null ? t('common.unlimited') : formatUsdMicros(limit);
}
</script>

<template>
  <div class="flex min-h-0 flex-1 flex-col overflow-hidden">
    <PageHeader :title="t('nav.tokens')" />

    <InlineError
      v-if="tokensQuery.isError.value && !tokensQuery.data.value"
      :message="extractApiError(tokensQuery.error.value).message"
      @retry="() => tokensQuery.refetch()"
    />

    <div v-else class="flex min-h-0 flex-1 flex-col overflow-hidden">
      <DataTable fill-viewport class="min-h-0 flex-1" :busy="showTableSkeleton">
        <template #toolbar>
          <DataTableToolbar>
            <FormTextInput
              id="tokens-search"
              v-model="searchText"
              type="text"
              class="h-8 max-w-xs"
              data-testid="tokens-search"
              :placeholder="t('tokens.search')"
              :aria-label="t('tokens.search')"
            />
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
          </DataTableToolbar>
        </template>
        <TableHeader>
          <TableRow>
            <TableHead>{{ t('tokens.name') }}</TableHead>
            <TableHead align="center">{{ t('tokens.key') }}</TableHead>
            <TableHead>{{ t('tokens.balance') }}</TableHead>
            <TableHead>{{ t('tokens.settled') }}</TableHead>
            <TableHead align="center">{{ t('tokens.limit') }}</TableHead>
            <TableHead align="center">{{ t('common.actions') }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRowsSkeleton v-if="showTableSkeleton" :columns="6" />
          <template v-else>
            <TableRow
              v-for="token in filteredTokens"
              :key="token.token_key"
              data-testid="token-row"
              :data-token-key="token.token_key"
            >
              <TableCell class="font-medium">{{ token.name }}</TableCell>
              <TableCell align="center">
                <code class="code-chip rounded px-2 py-0.5 font-mono text-xs">
                  {{ token.token_key }}
                </code>
              </TableCell>
              <TableCell class="font-mono" data-testid="token-balance">
                {{ formatUsdMicros(token.balance_usd_micros) }}
              </TableCell>
              <TableCell class="font-mono" data-testid="token-settled">
                {{ formatUsdMicros(token.settled_usd_micros) }}
              </TableCell>
              <TableCell align="center" class="font-mono">
                {{ formatLimit(token.limit_usd_micros) }}
              </TableCell>
              <TableCell align="center">
                <DataTableRowActions>
                  <DataTableMenuItem
                    data-testid="token-recharge"
                    @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                    @select="openBalance(token, 'recharge')"
                  >
                    {{ t('tokens.recharge') }}
                  </DataTableMenuItem>
                  <DataTableMenuItem
                    data-testid="token-deduct"
                    @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                    @select="openBalance(token, 'deduct')"
                  >
                    {{ t('tokens.deduct') }}
                  </DataTableMenuItem>
                  <DataTableMenuItem
                    data-testid="token-edit"
                    @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                    @select="openEdit(token)"
                  >
                    {{ t('common.edit') }}
                  </DataTableMenuItem>
                  <DataTableMenuSeparator />
                  <DataTableMenuItem
                    danger
                    data-testid="token-delete"
                    @pointerup.capture="pendingAnchor = anchorFromEvent($event)"
                    @select="openDelete(token)"
                  >
                    {{ t('common.delete') }}
                  </DataTableMenuItem>
                </DataTableRowActions>
              </TableCell>
            </TableRow>
            <TableRow v-if="filteredTokens.length === 0">
              <TableCell :colspan="6" class="h-24 whitespace-normal">
                <EmptyState :title="t('common.emptyList')">
                  <button type="button" class="btn btn-primary" @click="openCreate">
                    {{ t('tokens.create') }}
                  </button>
                </EmptyState>
              </TableCell>
            </TableRow>
          </template>
        </TableBody>
      </DataTable>
    </div>

    <template v-for="(win, index) in windows" :key="win.id">
      <TokenEditorWindow
        v-if="win.payload.kind === 'editor'"
        :initial="win.payload.token"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
      />
      <TokenBalanceWindow
        v-else-if="win.payload.kind === 'balance'"
        :token="win.payload.token"
        :mode="win.payload.mode"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
      />
      <ConfirmWindow
        v-else
        :title="t('tokens.deleteTitle')"
        :message="t('tokens.deleteMessage', { name: win.payload.token.name })"
        :anchor="win.anchor"
        :stack-order="win.z"
        :cascade="index"
        :attention="win.attention"
        :topmost="win.id === topmostId"
        :error="deleteErrors[win.id] ?? ''"
        :busy="deletingKey === win.payload.token.token_key"
        confirm-test-id="token-delete-confirm"
        @close="closeWindow(win.id)"
        @raise="bringToFront(win.id)"
        @dirty-change="(dirty) => setDirty(win.id, dirty)"
        @confirm="deleteMutation.mutate(win.payload.token.token_key)"
      />
    </template>
  </div>
</template>
