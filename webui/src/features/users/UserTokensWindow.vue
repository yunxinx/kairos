<script setup lang="ts">
import { computed, ref } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import type { TokenView, UserAdminView } from '@/api/types';
import EmptyState from '@/components/ui/EmptyState.vue';
import InlineError from '@/components/ui/InlineError.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import TableRowsSkeleton from '@/components/ui/table/TableRowsSkeleton.vue';
import { useToast } from '@/composables/useToast';
import { formatUsdFixed2, formatUsdMicros } from '@/lib/format';
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

const tokensQuery = useQuery({
  queryKey: ['users', props.user.id, 'tokens'],
  queryFn: () => apiClient.listUserTokens(props.user.id),
});

const tokens = computed(() => tokensQuery.data.value ?? []);
const showSkeleton = computed(() => tokensQuery.isPending.value && !tokensQuery.data.value);

const REMAINING_WARN_RATIO = 0.5;
const REMAINING_DANGER_RATIO = 0.2;

function quotaRatio(token: TokenView): number | null {
  const limit = token.limit_usd_micros;
  if (limit === null || limit <= 0) return null;
  return Math.min(1, Math.max(0, token.settled_usd_micros / limit));
}

function quotaColorClass(ratio: number): string {
  if (ratio >= 1 - REMAINING_DANGER_RATIO) return 'bg-[var(--danger)]';
  if (ratio >= 1 - REMAINING_WARN_RATIO) return 'bg-[var(--warn)]';
  return 'bg-[var(--success)]';
}

function quotaLabel(token: TokenView): string {
  const settled = formatUsdMicros(token.settled_usd_micros);
  if (token.limit_usd_micros === null) {
    return t('tokens.quotaUnlimitedUsage', { settled });
  }
  return t('tokens.quotaUsage', {
    settled,
    limit: formatUsdMicros(token.limit_usd_micros),
  });
}

const togglingId = ref<number | null>(null);
const toggleMutation = useMutation({
  mutationFn: async (token: TokenView) => {
    togglingId.value = token.id;
    await apiClient.setTokenEnabled(token.id, !token.enabled);
  },
  onSuccess: async () => {
    await queryClient.invalidateQueries({ queryKey: ['users', props.user.id, 'tokens'] });
    await queryClient.invalidateQueries({ queryKey: ['tokens'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
  onSettled: () => {
    togglingId.value = null;
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
              :key="token.id"
              data-testid="user-token-row"
              :data-token-id="token.id"
            >
              <TableCell class="max-w-32 font-medium">
                <span class="block truncate" :title="token.name">{{ token.name }}</span>
              </TableCell>
              <!-- 接口只返回脱敏 key：运营按 id 操作，不需要（也拿不到）明文。 -->
              <TableCell>
                <span class="text-fg-muted font-mono text-xs">{{ token.token_key }}</span>
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
                    <span v-if="token.limit_usd_micros !== null" data-testid="token-limit">
                      / {{ formatUsdFixed2(token.limit_usd_micros) }}
                    </span>
                  </div>
                  <div
                    v-if="quotaRatio(token) !== null"
                    class="bg-surface-alt h-1.5 w-full overflow-hidden rounded-full"
                    data-testid="token-quota-track"
                  >
                    <div
                      class="h-full rounded-full transition-[width]"
                      :class="quotaColorClass(quotaRatio(token) ?? 0)"
                      :style="{ width: `${(quotaRatio(token) ?? 0) * 100}%` }"
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
                  :disabled="togglingId === token.id"
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
