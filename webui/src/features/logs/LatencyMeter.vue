<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import {
  evaluateLatencyHealth,
  formatLatencyDisplay,
  formatThroughput,
  type LatencyHealth,
} from '@/lib/format';

const props = defineProps<{
  latencyMs: number;
  outputTokens: number;
  latencyTestId?: string;
  speedTestId?: string;
}>();

const { t } = useI18n();

const LATENCY_TONE: Record<LatencyHealth, { bar: string; value: string }> = {
  excellent: {
    bar: 'bg-emerald-600 dark:bg-emerald-500',
    value: 'text-emerald-700 dark:text-emerald-400',
  },
  normal: {
    bar: 'bg-amber-500',
    value: 'text-amber-700 dark:text-amber-400',
  },
  slow: {
    bar: 'bg-rose-500',
    value: 'text-rose-600 dark:text-rose-400',
  },
};

const health = computed(() => evaluateLatencyHealth(props.latencyMs, props.outputTokens));

const throughput = computed(() => formatThroughput(props.outputTokens, props.latencyMs));

const tone = computed(() => LATENCY_TONE[health.value]);
</script>

<template>
  <div class="flex items-stretch gap-1.5 font-mono text-xs whitespace-nowrap">
    <span class="w-[3px] shrink-0 rounded-full" :class="tone.bar" aria-hidden="true" />
    <div class="flex min-w-0 flex-col justify-center gap-0.5">
      <div class="flex items-baseline justify-between gap-2">
        <span class="text-fg-muted shrink-0">{{ t('logs.latency') }}</span>
        <span class="font-medium tabular-nums" :class="tone.value" :data-testid="latencyTestId">
          {{ formatLatencyDisplay(latencyMs) }}
        </span>
      </div>
      <div class="flex items-baseline justify-between gap-2">
        <span class="text-fg-muted shrink-0">{{ t('logs.speed') }}</span>
        <span class="tabular-nums" :class="tone.value" :data-testid="speedTestId">
          {{ throughput ?? t('common.emptyCell') }}
        </span>
      </div>
    </div>
  </div>
</template>
