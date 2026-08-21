<script setup lang="ts">
import { computed, ref } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import { tokenWriteBody, type TokenView, type UserAdminView } from '@/api/types';
import EmptyState from '@/components/ui/EmptyState.vue';
import InlineError from '@/components/ui/InlineError.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import { useToast } from '@/composables/useToast';
import { formatUsdFixed2, formatUsdMicros, maskTokenKey } from '@/lib/format';
import { groupDisplayName, tokenGroupUsable } from '@/lib/visible-models';

const props = defineProps<{
  user: UserAdminView;
}>();

const emit = defineEmits<{
  close: [];
}>();

const { t } = useI18n();
const { error } = useToast();
const queryClient = useQueryClient();

const copiedKey = ref<string | null>(null);

async function copyKey(key: string) {
  try {
    await navigator.clipboard.writeText(key);
    copiedKey.value = key;
    setTimeout(() => {
      if (copiedKey.value === key) copiedKey.value = null;
    }, 1500);
  } catch {
    // 忽略剪贴板写入失败
  }
}

const tokensQuery = useQuery({
  queryKey: ['users', props.user.id, 'tokens'],
  queryFn: () => apiClient.listUserTokens(props.user.id),
});

const tokens = computed(() => tokensQuery.data.value ?? []);
const showSkeleton = computed(() => tokensQuery.isPending.value && !tokensQuery.data.value);

const REMAINING_WARN_RATIO = 0.5;
const REMAINING_DANGER_RATIO = 0.2;

function quotaRatio(token: TokenView): number {
  const total = token.settled_usd_micros + props.user.balance_usd_micros;
  if (total <= 0) return 0;
  return Math.min(1, Math.max(0, props.user.balance_usd_micros / total));
}

function quotaColorClass(ratio: number): string {
  if (ratio <= REMAINING_DANGER_RATIO) return 'bg-[var(--danger)]';
  if (ratio <= REMAINING_WARN_RATIO) return 'bg-[var(--warn)]';
  return 'bg-[var(--success)]';
}

function quotaLabel(token: TokenView): string {
  return t('tokens.quotaUsage', {
    settled: formatUsdMicros(token.settled_usd_micros),
    balance: formatUsdMicros(props.user.balance_usd_micros),
  });
}

const togglingKey = ref<string | null>(null);
const toggleMutation = useMutation({
  mutationFn: async (token: TokenView) => {
    togglingKey.value = token.token_key;
    await apiClient.updateToken(
      token.token_key,
      tokenWriteBody({ ...token, enabled: !token.enabled }),
    );
  },
  onSuccess: async () => {
    await queryClient.invalidateQueries({ queryKey: ['users', props.user.id, 'tokens'] });
    await queryClient.invalidateQueries({ queryKey: ['tokens'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
  onSettled: () => {
    togglingKey.value = null;
  },
});
</script>

<template>
  <div>
    <div class="card-body space-y-3">
      <InlineError
        v-if="tokensQuery.isError.value && !tokensQuery.data.value"
        :message="extractApiError(tokensQuery.error.value).message"
        @retry="() => tokensQuery.refetch()"
      />
      <div v-else-if="showSkeleton" class="space-y-2">
        <TableRowsSkeleton :columns="5" />
      </div>
      <EmptyState v-else-if="tokens.length === 0" :title="t('users.tokensEmpty')" />
      <div
        v-else
        class="border-seed seed-scrollbar max-h-80 overflow-y-auto rounded-md border"
      >
        <table class="w-full border-collapse text-left text-xs">
          <TableHeader>
            <TableRow>
              <TableHead>{{ t('tokens.name') }}</TableHead>
              <TableHead>{{ t('tokens.key') }}</TableHead>
              <TableHead>{{ t('tokens.modelGroup') }}</TableHead>
              <TableHead class="w-32">{{ t('tokens.quota') }}</TableHead>
              <TableHead align="center" class="w-20">{{ t('tokens.status') }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow
              v-for="token in tokens"
              :key="token.token_key"
              data-testid="user-token-row"
              :data-token-key="token.token_key"
            >
              <TableCell class="max-w-32 font-medium">
                <span class="block truncate" :title="token.name">{{ token.name }}</span>
              </TableCell>
              <TableCell>
                <div class="flex items-center gap-1.5 font-mono text-xs">
                  <span class="text-fg-muted">{{ maskTokenKey(token.token_key) }}</span>
                  <button
                    type="button"
                    class="btn btn-ghost btn-icon size-5 text-xs"
                    data-testid="token-copy-key"
                    :aria-label="copiedKey === token.token_key ? t('common.copied') : t('common.copy')"
                    :title="copiedKey === token.token_key ? t('common.copied') : t('common.copy')"
                    @click="copyKey(token.token_key)"
                  >
                    <UiIcon
                      :name="copiedKey === token.token_key ? 'check' : 'copy'"
                      :size="12"
                      :class="copiedKey === token.token_key ? 'text-success' : undefined"
                    />
                  </button>
                </div>
              </TableCell>
              <TableCell class="font-mono text-xs" data-testid="user-token-model-group">
                <span class="inline-flex items-center gap-1">
                  {{ groupDisplayName(token.model_group, t('models.ungrouped')) }}
                  <span
                    v-if="!tokenGroupUsable(token.model_group, user.role, user.assigned_groups)"
                    class="badge badge-danger text-[10px]"
                    data-testid="token-group-unusable"
                    :title="t('tokens.groupUnusableHint')"
                  >
                    {{ t('tokens.groupUnusable') }}
                  </span>
                </span>
              </TableCell>
              <TableCell>
                <div class="w-28" :title="quotaLabel(token)">
                  <div class="text-fg-muted mb-1 flex justify-between font-mono text-xs">
                    <span data-testid="token-settled">{{
                      formatUsdFixed2(token.settled_usd_micros)
                    }}</span>
                    <span data-testid="token-balance">{{
                      formatUsdFixed2(user.balance_usd_micros)
                    }}</span>
                  </div>
                  <div
                    class="bg-surface-alt h-1.5 w-full overflow-hidden rounded-full"
                    data-testid="token-quota-track"
                  >
                    <div
                      class="h-full rounded-full transition-[width]"
                      :class="quotaColorClass(quotaRatio(token))"
                      :style="{ width: `${quotaRatio(token) * 100}%` }"
                    />
                  </div>
                </div>
              </TableCell>
              <TableCell align="center">
                <button
                  type="button"
                  class="badge cursor-pointer text-[10px]"
                  :class="token.enabled ? 'badge-success' : 'badge-danger'"
                  data-testid="user-token-toggle-enabled"
                  :disabled="togglingKey === token.token_key"
                  :aria-label="token.enabled ? t('tokens.disable') : t('tokens.enable')"
                  :title="token.enabled ? t('tokens.disable') : t('tokens.enable')"
                  @click="toggleMutation.mutate(token)"
                >
                  {{ token.enabled ? t('tokens.statusEnabled') : t('tokens.statusDisabled') }}
                </button>
              </TableCell>
            </TableRow>
          </TableBody>
        </table>
      </div>
    </div>
    <div class="card-footer card-body flex justify-end">
      <button type="button" class="btn" @click="emit('close')">{{ t('common.close') }}</button>
    </div>
  </div>
</template>
