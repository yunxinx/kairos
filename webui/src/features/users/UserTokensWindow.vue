<script setup lang="ts">
import { computed } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import { tokenWriteBody, type TokenView, type UserAdminView } from '@/api/types';
import InlineError from '@/components/ui/InlineError.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import { useToast } from '@/composables/useToast';
import { maskTokenKey } from '@/lib/format';
import { tokenGroupUsable } from '@/lib/visible-models';

const props = defineProps<{
  user: UserAdminView;
}>();

const { t } = useI18n();
const { error } = useToast();
const queryClient = useQueryClient();

const tokensQuery = useQuery({
  queryKey: ['users', props.user.id, 'tokens'],
  queryFn: () => apiClient.listUserTokens(props.user.id),
});

const tokens = computed(() => tokensQuery.data.value ?? []);

const disableMutation = useMutation({
  mutationFn: (token: TokenView) =>
    apiClient.updateToken(token.token_key, tokenWriteBody({ ...token, enabled: false })),
  onSuccess: async () => {
    await queryClient.invalidateQueries({ queryKey: ['users', props.user.id, 'tokens'] });
    await queryClient.invalidateQueries({ queryKey: ['tokens'] });
  },
  onError: (err) => {
    error(extractApiError(err).message);
  },
});
</script>

<template>
  <div class="card-body space-y-3">
    <InlineError
      v-if="tokensQuery.isError.value && !tokensQuery.data.value"
      :message="extractApiError(tokensQuery.error.value).message"
      @retry="() => tokensQuery.refetch()"
    />
    <EmptyState v-else-if="tokens.length === 0" :title="t('users.tokensEmpty')" />
    <ul v-else class="divide-seed divide-y">
      <li
        v-for="token in tokens"
        :key="token.token_key"
        class="flex items-center justify-between gap-3 py-2"
        data-testid="user-token-row"
        :data-token-key="token.token_key"
      >
        <div class="min-w-0">
          <p class="truncate text-sm font-medium">{{ token.name }}</p>
          <p class="text-fg-muted font-mono text-xs">{{ maskTokenKey(token.token_key) }}</p>
          <p
            v-if="!tokenGroupUsable(token.model_group, user.role, user.assigned_groups)"
            class="text-danger mt-1 text-xs"
            data-testid="token-group-unusable"
          >
            {{ t('tokens.groupUnusableHint') }}
          </p>
        </div>
        <button
          v-if="token.enabled"
          type="button"
          class="btn btn-ghost text-xs"
          data-testid="user-token-disable"
          :disabled="disableMutation.isPending.value"
          @click="disableMutation.mutate(token)"
        >
          {{ t('tokens.disable') }}
        </button>
        <span v-else class="text-fg-muted text-xs">{{ t('tokens.statusDisabled') }}</span>
      </li>
    </ul>
  </div>
</template>
