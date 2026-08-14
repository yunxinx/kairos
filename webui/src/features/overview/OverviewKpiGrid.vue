<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import type { StatsSummary } from '@/api/types';
import SkeletonBlock from '@/components/ui/SkeletonBlock.vue';
import { formatCount, formatPercent, formatTokensMillions, formatUsdMicros } from '@/lib/format';

const props = defineProps<{
  summary: StatsSummary | null;
}>();

const { t, locale } = useI18n();

const successRatio = computed(() => {
  const summary = props.summary;
  if (!summary) return 0;
  const total = summary.request_count;
  if (total <= 0) return 0;
  return summary.success_count / total;
});

const tokenTotal = computed(() => {
  const summary = props.summary;
  if (!summary) return 0;
  return summary.input_tokens + summary.output_tokens;
});
</script>

<template>
  <section
    :role="summary ? undefined : 'status'"
    :aria-label="summary ? undefined : t('common.loading')"
  >
    <div class="grid grid-cols-1 gap-3 sm:grid-cols-3">
      <div class="card">
        <div class="card-body">
          <div class="flex items-baseline justify-between gap-2">
            <div class="text-fg-muted text-xs font-medium">{{ t('overview.requests') }}</div>
            <div v-if="summary" class="text-fg-subtle font-mono text-xs font-normal">
              <span data-testid="overview-token-count">{{
                formatCount(summary.token_count, locale)
              }}</span>
              {{ t('overview.tokenCount') }}
              <span aria-hidden="true"> · </span>
              <span data-testid="overview-channel-count">{{
                formatCount(summary.channel_count, locale)
              }}</span>
              {{ t('overview.channelCount') }}
            </div>
            <SkeletonBlock v-else height="h-3" width="w-32" />
          </div>
          <div v-if="summary" class="mt-2 font-mono text-2xl font-bold">
            <span data-testid="overview-request-count">{{
              formatCount(summary.request_count, locale)
            }}</span>
          </div>
          <SkeletonBlock v-else height="h-8" width="w-24" class="mt-2" />
          <p v-if="summary" class="text-fg-muted mt-1 text-xs">
            <span data-testid="overview-success-count">{{
              formatCount(summary.success_count, locale)
            }}</span>
            {{ t('overview.successRate', { rate: formatPercent(successRatio, locale) }) }}
          </p>
          <SkeletonBlock v-else height="h-3" width="w-32" class="mt-1" />
        </div>
      </div>

      <div class="card">
        <div class="card-body">
          <div class="text-fg-muted text-xs font-medium">{{ t('overview.cost') }}</div>
          <div v-if="summary" class="text-primary mt-2 font-mono text-2xl font-bold">
            <span data-testid="overview-cost">{{ formatUsdMicros(summary.cost_usd_micros) }}</span>
          </div>
          <SkeletonBlock v-else height="h-8" width="w-28" class="mt-2" />
        </div>
      </div>

      <div class="card">
        <div class="card-body">
          <div class="flex items-baseline justify-between gap-2">
            <div class="text-fg-muted text-xs font-medium">{{ t('overview.tokensTotal') }}</div>
            <div
              v-if="summary"
              class="text-fg-subtle font-mono text-xs font-normal"
              data-testid="overview-tokens-millions"
            >
              {{ formatTokensMillions(tokenTotal) }}
            </div>
            <SkeletonBlock v-else height="h-3" width="w-16" />
          </div>
          <div v-if="summary" class="mt-2 font-mono text-2xl font-bold">
            {{ formatCount(tokenTotal, locale) }}
          </div>
          <SkeletonBlock v-else height="h-8" width="w-24" class="mt-2" />
          <p v-if="summary" class="text-fg-muted mt-1 font-mono text-xs">
            <span data-testid="overview-input-tokens">{{
              formatTokensMillions(summary.input_tokens)
            }}</span>
            {{ t('overview.inputShort') }}
            <span aria-hidden="true"> · </span>
            <span data-testid="overview-output-tokens">{{
              formatTokensMillions(summary.output_tokens)
            }}</span>
            {{ t('overview.outputShort') }}
          </p>
          <SkeletonBlock v-else height="h-3" width="w-40" class="mt-1" />
        </div>
      </div>
    </div>
  </section>
</template>
