<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import type { LifetimeStats } from '@/api/types';
import SkeletonBlock from '@/components/ui/SkeletonBlock.vue';
import { formatCount, formatTokensMillions, formatUsdMicros } from '@/lib/format';

withDefaults(
  defineProps<{
    lifetime?: LifetimeStats | null;
    lifetimeLoading?: boolean;
    lifetimeError?: string;
  }>(),
  {
    lifetime: null,
    lifetimeLoading: false,
    lifetimeError: '',
  },
);

const emit = defineEmits<{
  retryLifetime: [];
}>();

const { t, locale } = useI18n();
</script>

<template>
  <div class="card overview-lifetime-card">
    <div class="card-body overview-lifetime-body" data-testid="overview-lifetime">
      <div v-if="lifetimeError" class="overview-lifetime-error">
        <p class="text-danger text-sm">{{ lifetimeError }}</p>
        <button type="button" class="btn btn-sm" @click="emit('retryLifetime')">
          {{ t('common.retry') }}
        </button>
      </div>
      <template v-else>
        <div>
          <div class="overview-lifetime-label">{{ t('overview.lifetimeRequests') }}</div>
          <div
            v-if="lifetime"
            class="overview-lifetime-value"
            data-testid="overview-lifetime-requests"
          >
            {{ formatCount(lifetime.request_count, locale) }}
          </div>
          <SkeletonBlock v-else-if="lifetimeLoading" height="h-6" width="w-16" class="mt-1" />
        </div>
        <div>
          <div class="overview-lifetime-label">{{ t('overview.lifetimeCost') }}</div>
          <div v-if="lifetime" class="overview-lifetime-value" data-testid="overview-lifetime-cost">
            {{ formatUsdMicros(lifetime.cost_usd_micros) }}
          </div>
          <SkeletonBlock v-else-if="lifetimeLoading" height="h-6" width="w-20" class="mt-1" />
        </div>
        <div>
          <div class="overview-lifetime-label">{{ t('overview.lifetimeTokens') }}</div>
          <div
            v-if="lifetime"
            class="overview-lifetime-value"
            data-testid="overview-lifetime-tokens"
          >
            {{ formatTokensMillions(lifetime.total_tokens) }}
          </div>
          <SkeletonBlock v-else-if="lifetimeLoading" height="h-6" width="w-20" class="mt-1" />
        </div>
      </template>
    </div>
  </div>
</template>
