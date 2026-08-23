<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { isProtocol, type LogEntry } from '@/api/types';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import ProtocolBadge from '@/components/ui/ProtocolBadge.vue';
import LogBodyPanel from '@/features/logs/LogBodyPanel.vue';
import LatencyMeter from '@/features/logs/LatencyMeter.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import {
  formatUnixMillis,
  formatUsdMicros,
  formatUsdAmount,
  formatTokensCount,
  maskTokenKey,
  componentCostMicros,
  formatDiscountBp,
} from '@/lib/format';
import { resolveOutboundProtocol } from '@/lib/protocol';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const props = withDefaults(
  defineProps<{
    entry: LogEntry;
    detail?: LogEntry | null;
    detailLoading?: boolean;
    detailError?: string;
    closing?: boolean;
    /** 是否显示补扣/豁免入口。后端要求 admin+，普通用户点了只会拿到 403。 */
    canSettle?: boolean;
    channelProtocolMap?: Map<string, string> | null;
    anchor?: FloatingWindowAnchor | null;
    stackOrder?: number;
    cascade?: number;
    attention?: boolean;
    topmost?: boolean;
  }>(),
  {
    detail: null,
    detailLoading: false,
    detailError: '',
    closing: false,
    canSettle: false,
    channelProtocolMap: null,
    anchor: null,
    stackOrder: 0,
    cascade: 0,
    attention: false,
    topmost: true,
  },
);

const emit = defineEmits<{
  close: [];
  raise: [];
  settle: [id: number];
  waive: [id: number];
  filterModel: [model: string];
  filterChannel: [channel: string];
  filterToken: [token: string];
  retryDetail: [];
}>();

const { t, locale } = useI18n();

const windowTitle = computed(() => `#${props.entry.id} · ${props.entry.model}`);

function statusBadgeClass(statusCode: number): string {
  if (statusCode >= 200 && statusCode < 300) return 'badge-success';
  if (statusCode >= 400 && statusCode < 500) return 'badge-warn';
  return 'badge-danger';
}

const outbound = computed(() =>
  resolveOutboundProtocol(
    props.entry.inbound_protocol,
    props.entry.channel,
    props.channelProtocolMap,
  ),
);

function protocolTitle(proto: string): string {
  return isProtocol(proto) ? t(`protocol.${proto}`) : proto;
}

const calculationSteps = computed(() => {
  const steps = [
    {
      name: t('logs.promptTokens'),
      tokens: props.entry.input_tokens,
      price: props.entry.input_price_usd_micros,
      subtotalMicros: componentCostMicros(
        props.entry.input_tokens,
        props.entry.input_price_usd_micros,
      ),
      isCache: false,
    },
    {
      name: t('logs.completionTokens'),
      tokens: props.entry.output_tokens,
      price: props.entry.output_price_usd_micros,
      subtotalMicros: componentCostMicros(
        props.entry.output_tokens,
        props.entry.output_price_usd_micros,
      ),
      isCache: false,
    },
  ];
  if (props.entry.cache_read_tokens > 0) {
    steps.push({
      name: t('logs.cacheReadTokens'),
      tokens: props.entry.cache_read_tokens,
      price: props.entry.cache_read_price_usd_micros,
      subtotalMicros: componentCostMicros(
        props.entry.cache_read_tokens,
        props.entry.cache_read_price_usd_micros,
      ),
      isCache: true,
    });
  }
  if (props.entry.cache_write_tokens > 0) {
    steps.push({
      name: t('logs.cacheWriteTokens'),
      tokens: props.entry.cache_write_tokens,
      price: props.entry.cache_write_price_usd_micros,
      subtotalMicros: componentCostMicros(
        props.entry.cache_write_tokens,
        props.entry.cache_write_price_usd_micros,
      ),
      isCache: true,
    });
  }
  return steps;
});
</script>

<template>
  <FloatingWindow
    :title="windowTitle"
    extra-wide
    :anchor="anchor"
    :stack-order="stackOrder"
    :cascade="cascade"
    :attention="attention"
    :topmost="topmost"
    data-testid="request-log-detail-window"
    @close="emit('close')"
    @pointerdown="emit('raise')"
  >
    <template #header-extra>
      <div class="mr-2 ml-auto flex items-center gap-1.5">
        <div
          v-if="outbound.status === 'converted'"
          class="inline-flex items-center gap-1"
          :title="`${t('logs.protocolConversion')}: ${protocolTitle(entry.inbound_protocol)} ⇄ ${protocolTitle(outbound.protocol)}`"
        >
          <ProtocolBadge :protocol="entry.inbound_protocol" />
          <UiIcon name="arrow-left-right" :size="11" class="text-fg-muted shrink-0 opacity-80" />
          <ProtocolBadge :protocol="outbound.protocol" />
        </div>
        <div
          v-else-if="outbound.status === 'unknown'"
          class="inline-flex items-center gap-1"
          data-testid="log-detail-outbound-protocol-unknown"
        >
          <ProtocolBadge :protocol="entry.inbound_protocol" />
          <UiIcon name="arrow-left-right" :size="11" class="text-fg-muted shrink-0 opacity-80" />
          <span class="badge badge-neutral w-fit text-[10px]">{{
            t('logs.outboundProtocolUnknown')
          }}</span>
        </div>
        <ProtocolBadge v-else :protocol="entry.inbound_protocol" />

        <span class="badge text-[11px]" :class="statusBadgeClass(entry.status_code)">
          {{ entry.status_code }}
        </span>
      </div>
    </template>

    <div class="flex flex-col gap-4 p-4">
      <div class="grid gap-3.5 lg:grid-cols-5">
        <div
          class="flex flex-col justify-between rounded-lg border border-[var(--seed-border)] bg-[var(--seed-surface)] p-3.5 shadow-xs lg:col-span-2"
          data-testid="log-detail-meta"
        >
          <div>
            <div
              class="mb-2.5 flex items-center justify-between border-b border-[var(--seed-border)]/50 pb-2"
            >
              <h4 class="text-xs font-bold tracking-wider text-[var(--fg-muted)] uppercase">
                {{ t('logs.expand') }}
              </h4>
              <span class="font-mono text-[11px] text-[var(--fg-muted)]">
                {{ formatUnixMillis(entry.created_at, locale) }}
              </span>
            </div>
            <dl class="grid grid-cols-2 gap-x-3 gap-y-2.5 text-xs">
              <div class="col-span-2 sm:col-span-1">
                <dt class="text-fg-muted flex items-center gap-1 text-[11px]">
                  <span>{{ t('logs.model') }}</span>
                  <button
                    type="button"
                    class="text-[var(--fg-muted)] hover:text-[var(--seed-primary)]"
                    :title="t('logs.filterByModel')"
                    @click="emit('filterModel', entry.model)"
                  >
                    <UiIcon name="filter" :size="11" />
                  </button>
                </dt>
                <dd class="mt-0.5 font-mono font-semibold break-all text-[var(--seed-fg)]">
                  {{ entry.model }}
                </dd>
                <div
                  v-if="entry.outbound_model && entry.outbound_model !== entry.model"
                  class="text-fg-muted mt-0.5 flex items-center gap-0.5 font-mono text-[10px] opacity-85"
                >
                  <UiIcon name="arrow-right" :size="9" />
                  <span data-testid="log-outbound-model">{{ entry.outbound_model }}</span>
                </div>
              </div>
              <div class="col-span-2 sm:col-span-1">
                <dt class="text-fg-muted flex items-center gap-1 text-[11px]">
                  <span>{{ t('logs.channel') }}</span>
                  <button
                    type="button"
                    class="text-[var(--fg-muted)] hover:text-[var(--seed-primary)]"
                    :title="t('logs.filterByChannel')"
                    @click="emit('filterChannel', entry.channel)"
                  >
                    <UiIcon name="filter" :size="11" />
                  </button>
                </dt>
                <dd class="mt-0.5 font-mono" data-testid="log-detail-channel">
                  <span class="code-chip rounded px-1.5 py-0.5 text-[11px]">{{
                    entry.channel
                  }}</span>
                </dd>
              </div>
              <div class="col-span-2">
                <dt class="text-fg-muted flex items-center gap-1 text-[11px]">
                  <span>{{ t('logs.token') }}</span>
                  <button
                    type="button"
                    class="text-[var(--fg-muted)] hover:text-[var(--seed-primary)]"
                    :title="t('logs.filterByToken')"
                    @click="emit('filterToken', entry.token_name)"
                  >
                    <UiIcon name="filter" :size="11" />
                  </button>
                </dt>
                <dd
                  class="mt-0.5 flex items-center gap-1.5 font-mono text-xs"
                  :title="`${entry.token_name} (${entry.token_key})`"
                >
                  <span class="font-medium text-[var(--seed-fg)]">{{ entry.token_name }}</span>
                  <span class="text-fg-muted text-[10px] opacity-75"
                    >({{ maskTokenKey(entry.token_key) }})</span
                  >
                </dd>
              </div>
              <div>
                <dt class="text-fg-muted text-[11px]">{{ t('logs.latencyAndSpeed') }}</dt>
                <dd class="mt-1">
                  <LatencyMeter
                    :latency-ms="entry.latency_ms"
                    :output-tokens="entry.output_tokens"
                  />
                </dd>
              </div>
              <div>
                <dt class="text-fg-muted text-[11px]">{{ t('logs.tokens') }}</dt>
                <dd class="mt-0.5 font-mono text-xs">
                  <div class="flex flex-col gap-0.5">
                    <span class="text-fg-muted" :title="t('logs.promptTokens')">
                      ↑ {{ formatTokensCount(entry.input_tokens) }}
                    </span>
                    <span
                      class="font-semibold text-[var(--seed-fg)]"
                      :title="t('logs.completionTokens')"
                    >
                      ↓ {{ formatTokensCount(entry.output_tokens) }}
                    </span>
                  </div>
                </dd>
              </div>
            </dl>
          </div>
        </div>

        <div
          class="flex flex-col justify-between rounded-lg border border-[var(--seed-border)] bg-[var(--seed-surface)] p-3.5 shadow-xs lg:col-span-3"
        >
          <div>
            <div
              class="mb-2.5 flex items-center justify-between border-b border-[var(--seed-border)]/50 pb-2"
            >
              <h4 class="text-xs font-bold tracking-wider text-[var(--fg-muted)] uppercase">
                {{ t('logs.costCalculation') }}
              </h4>
              <span
                class="badge text-[10px]"
                :class="entry.settled ? 'badge-success' : 'badge-warn'"
              >
                {{ entry.settled ? t('logs.settledYes') : t('logs.settledNo') }}
              </span>
            </div>

            <div class="overflow-x-auto">
              <table class="w-full text-left font-mono text-xs">
                <thead>
                  <tr class="text-fg-muted border-b border-[var(--seed-border)] text-[11px]">
                    <th class="pb-1 font-medium">{{ t('pricing.model') }}</th>
                    <th class="pb-1 font-medium">{{ t('logs.tokensUnit') }}</th>
                    <th class="pb-1 font-medium">{{ t('logs.pricingPerMillion') }}</th>
                    <th class="pb-1 text-right font-medium">{{ t('logs.subtotal') }}</th>
                  </tr>
                </thead>
                <tbody class="text-xs">
                  <tr v-for="step in calculationSteps" :key="step.name">
                    <td
                      class="py-1"
                      :class="step.isCache ? 'text-blue-600 dark:text-blue-400' : 'text-fg-muted'"
                    >
                      {{ step.name }}
                    </td>
                    <td class="py-1">{{ step.tokens.toLocaleString() }}</td>
                    <td class="py-1">${{ formatUsdAmount(step.price) }}</td>
                    <td class="py-1 text-right font-medium text-[var(--seed-fg)]">
                      {{ formatUsdMicros(step.subtotalMicros) }}
                    </td>
                  </tr>
                </tbody>
                <tfoot>
                  <tr class="border-t border-[var(--seed-fg)]/70">
                    <td class="pt-1.5 font-medium text-[var(--seed-fg)]" colspan="3">
                      {{ t('logs.baseTotal') }}
                    </td>
                    <td class="pt-1.5 text-right font-medium text-[var(--seed-fg)]" data-testid="log-base-cost">
                      {{ formatUsdMicros(entry.base_cost_usd_micros) }}
                    </td>
                  </tr>
                  <tr>
                    <td class="pt-1.5 font-medium text-[var(--fg-muted)]" colspan="3">
                      {{ t('logs.discountRate') }}
                    </td>
                    <td class="pt-1.5 text-right font-medium text-[var(--fg-muted)]" data-testid="log-discount-rate">
                      {{ formatDiscountBp(entry.discount_bp) }}
                    </td>
                  </tr>
                  <tr>
                    <td class="pt-1.5 font-bold text-[var(--seed-fg)]" colspan="3">
                      {{ t('logs.chargedTotal') }}
                    </td>
                    <td class="pt-1.5 text-right text-sm font-bold text-[var(--seed-fg)]" data-testid="log-charged-cost">
                      {{ formatUsdMicros(entry.cost_usd_micros) }}
                    </td>
                  </tr>
                </tfoot>
              </table>
            </div>
          </div>

          <div
            v-if="!entry.settled && canSettle"
            class="mt-3 flex flex-wrap items-center gap-2 border-t border-[var(--seed-border)] pt-2.5"
            data-testid="log-unsettled-actions"
          >
            <span class="text-xs font-medium text-amber-600 dark:text-amber-400">
              {{ t('logs.unsettled') }}:
            </span>
            <button
              type="button"
              class="btn btn-sm btn-subtle"
              data-testid="log-settle"
              :disabled="closing"
              :title="t('logs.settleGuide')"
              @click="emit('settle', entry.id)"
            >
              {{ t('logs.settleCharge') }}
            </button>
            <button
              type="button"
              class="btn btn-sm btn-subtle"
              data-testid="log-waive"
              :disabled="closing"
              :title="t('logs.waiveGuide')"
              @click="emit('waive', entry.id)"
            >
              {{ t('logs.waiveCharge') }}
            </button>
          </div>
        </div>
      </div>

      <div
        class="rounded-lg border border-[var(--seed-border)] bg-[var(--seed-surface)] p-3.5 shadow-xs"
      >
        <LogBodyPanel v-if="detail" :entry="detail" />
        <p
          v-else-if="detailLoading"
          class="text-fg-muted py-6 text-center text-sm"
          data-testid="log-body-loading"
        >
          {{ t('logs.bodyLoading') }}
        </p>
        <div
          v-else-if="detailError"
          class="flex flex-col items-center gap-2 py-6"
          data-testid="log-body-error"
        >
          <p class="text-danger text-center text-sm">{{ detailError }}</p>
          <button type="button" class="btn btn-sm" @click="emit('retryDetail')">
            {{ t('common.retry') }}
          </button>
        </div>
      </div>
    </div>
  </FloatingWindow>
</template>
