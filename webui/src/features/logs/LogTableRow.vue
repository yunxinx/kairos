<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import type { LogEntry } from '@/api/types';
import ProtocolBadge from '@/components/ui/ProtocolBadge.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import LatencyMeter from '@/features/logs/LatencyMeter.vue';
import {
  formatUnixMillis,
  formatUsdMicros,
  formatTokensCount,
  computeCacheHitRatio,
} from '@/lib/format';
import { resolveOutboundProtocol } from '@/lib/protocol';

export type RequestLogVisibleColumns = {
  token: boolean;
  model: boolean;
  channel: boolean;
  inboundProtocol: boolean;
  tokens: boolean;
  latency: boolean;
  cache: boolean;
  cacheHit: boolean;
  cost: boolean;
  settled: boolean;
  body: boolean;
};

const props = withDefaults(
  defineProps<{
    entry: LogEntry;
    visible: RequestLogVisibleColumns;
    active?: boolean;
    channelProtocolMap?: Map<string, string> | null;
  }>(),
  {
    active: false,
    channelProtocolMap: null,
  },
);

const emit = defineEmits<{
  openBilling: [event: MouseEvent, entry: LogEntry];
  openBody: [event: MouseEvent, entry: LogEntry];
  filterModel: [model: string];
  filterChannel: [channel: string];
  filterToken: [token: string];
}>();

const { t, locale } = useI18n();

const outbound = computed(() =>
  resolveOutboundProtocol(
    props.entry.inbound_protocol,
    props.entry.channel,
    props.channelProtocolMap,
  ),
);

const isFailed = computed(() => props.entry.status_code >= 400);

const totalTokens = computed(
  () =>
    props.entry.input_tokens +
    props.entry.output_tokens +
    props.entry.cache_read_tokens +
    props.entry.cache_write_tokens,
);

const hasCache = computed(
  () => props.entry.cache_read_tokens > 0 || props.entry.cache_write_tokens > 0,
);

const cacheHitRatio = computed(() =>
  computeCacheHitRatio(props.entry.cache_read_tokens, props.entry.input_tokens),
);

/* 失败行整行淡红(log-row-failed);普通行的悬停/选中由 TableRow 原语渲染 */
const rowClass = computed(() => (isFailed.value ? 'log-row-failed' : ''));

function handleRowClick(event: MouseEvent) {
  if ((event.target as HTMLElement).closest('button')) return;
  emit('openBilling', event, props.entry);
}
</script>

<template>
  <TableRow
    data-testid="log-row"
    :data-log-id="String(entry.id)"
    :data-model="entry.model"
    :data-status-code="String(entry.status_code)"
    class="log-row group cursor-pointer transition-colors"
    :class="rowClass"
    :data-state="active ? 'selected' : undefined"
    @click="handleRowClick"
  >
    <TableCell class="text-fg-muted font-mono text-xs whitespace-nowrap">
      <div class="flex flex-col items-start gap-0.5">
        <span>{{ formatUnixMillis(entry.created_at, locale) }}</span>
        <!-- 未出站即终局的请求（余额拒绝、无可用渠道等）：仅在字段明确为 false 时渲染。 -->
        <span
          v-if="entry.dispatched === false"
          class="badge badge-neutral text-fg-muted w-fit px-1 py-0 font-medium"
          :title="t('logs.notDispatched')"
          data-testid="log-not-dispatched"
        >
          {{ t('logs.notDispatched') }}
        </span>
      </div>
    </TableCell>

    <TableCell v-if="visible.token">
      <div class="flex min-w-0 flex-col">
        <div class="flex min-w-0 items-center gap-1">
          <span class="truncate text-xs font-medium" :title="entry.token_name">
            {{ entry.token_name }}
          </span>
          <button
            type="button"
            class="shrink-0 rounded p-0.5 text-[var(--fg-muted)] opacity-0 transition-opacity group-hover:opacity-100 hover:text-[var(--seed-primary)]"
            data-testid="log-filter-token"
            :title="t('logs.filterByToken')"
            @click.stop="emit('filterToken', entry.token_name)"
          >
            <UiIcon name="filter" :size="11" />
          </button>
        </div>
        <div
          class="text-fg-muted text-3xs truncate font-mono opacity-80"
          :title="entry.token_key_masked"
        >
          {{ entry.token_key_masked }}
        </div>
      </div>
    </TableCell>

    <TableCell v-if="visible.model">
      <div class="flex min-w-0 flex-col">
        <div class="flex min-w-0 items-center gap-1" data-testid="log-model-container">
          <span
            class="truncate font-mono text-xs font-semibold text-[var(--seed-fg)]"
            data-testid="log-model"
          >
            {{ entry.model }}
          </span>
          <button
            type="button"
            class="shrink-0 rounded p-0.5 text-[var(--fg-muted)] opacity-0 transition-opacity group-hover:opacity-100 hover:text-[var(--seed-primary)]"
            data-testid="log-filter-model"
            :title="t('logs.filterByModel')"
            @click.stop="emit('filterModel', entry.model)"
          >
            <UiIcon name="filter" :size="11" />
          </button>
        </div>
        <div
          v-if="entry.outbound_model && entry.outbound_model !== entry.model"
          class="text-fg-muted text-3xs inline-flex items-center gap-0.5 truncate font-mono opacity-80"
          :title="`${t('logs.outboundModel')}: ${entry.outbound_model}`"
          data-testid="log-row-outbound-model"
        >
          <UiIcon name="arrow-right" :size="9" />
          <span>{{ entry.outbound_model }}</span>
        </div>
      </div>
    </TableCell>

    <TableCell v-if="visible.channel">
      <div class="inline-flex max-w-full items-center gap-1">
        <span
          class="code-chip text-2xs truncate rounded px-1.5 py-0.5 font-mono"
          data-testid="log-channel"
        >
          {{ entry.channel }}
        </span>
        <button
          type="button"
          class="shrink-0 rounded p-0.5 text-[var(--fg-muted)] opacity-0 transition-opacity group-hover:opacity-100 hover:text-[var(--seed-primary)]"
          data-testid="log-filter-channel"
          :title="t('logs.filterByChannel')"
          @click.stop="emit('filterChannel', entry.channel)"
        >
          <UiIcon name="filter" :size="11" />
        </button>
      </div>
    </TableCell>

    <TableCell v-if="visible.inboundProtocol">
      <div class="flex min-w-0 flex-col items-start gap-1" data-testid="log-inbound-protocol">
        <div v-if="outbound.status === 'converted'" class="flex flex-col gap-1">
          <div class="inline-flex items-center gap-1">
            <span class="badge badge-neutral text-fg-muted px-1 py-0 font-medium uppercase">{{
              t('logs.protoIn')
            }}</span>
            <ProtocolBadge :protocol="entry.inbound_protocol" />
          </div>
          <div
            class="inline-flex items-center gap-1"
            data-testid="log-row-outbound-protocol"
            :title="`${t('logs.protocolConversion')}: ${entry.inbound_protocol} → ${outbound.protocol}`"
          >
            <span class="badge badge-neutral text-fg-muted px-1 py-0 font-medium uppercase">{{
              t('logs.protoOut')
            }}</span>
            <ProtocolBadge :protocol="outbound.protocol" />
          </div>
        </div>
        <div v-else-if="outbound.status === 'unknown'" class="flex flex-col gap-1">
          <div class="inline-flex items-center gap-1">
            <span class="badge badge-neutral text-fg-muted px-1 py-0 font-medium uppercase">{{
              t('logs.protoIn')
            }}</span>
            <ProtocolBadge :protocol="entry.inbound_protocol" />
          </div>
          <div class="inline-flex items-center gap-1" data-testid="log-outbound-protocol-unknown">
            <span class="badge badge-neutral text-fg-muted px-1 py-0 font-medium uppercase">{{
              t('logs.protoOut')
            }}</span>
            <span class="badge badge-neutral w-fit">{{ t('logs.outboundProtocolUnknown') }}</span>
          </div>
        </div>
        <div v-else class="inline-flex items-center gap-1">
          <ProtocolBadge :protocol="entry.inbound_protocol" />
        </div>
      </div>
    </TableCell>

    <TableCell v-if="visible.tokens" class="font-mono text-xs">
      <div v-if="totalTokens > 0" class="flex flex-col gap-0.5">
        <span class="text-fg-muted" :title="t('logs.promptTokens')">
          ↑ {{ formatTokensCount(entry.input_tokens) }}
        </span>
        <span class="font-semibold text-[var(--seed-fg)]" :title="t('logs.completionTokens')">
          ↓ {{ formatTokensCount(entry.output_tokens) }}
        </span>
      </div>
      <span v-else class="text-fg-muted">0</span>
    </TableCell>

    <TableCell v-if="visible.latency">
      <LatencyMeter
        :latency-ms="entry.latency_ms"
        :output-tokens="entry.output_tokens"
        latency-test-id="log-latency"
        speed-test-id="log-speed"
      />
    </TableCell>

    <TableCell v-if="visible.cache" class="font-mono text-xs">
      <div v-if="hasCache" class="flex flex-col gap-0.5">
        <span v-if="entry.cache_read_tokens > 0" class="text-info">
          {{ t('logs.cacheReadShort') }} {{ formatTokensCount(entry.cache_read_tokens) }}
        </span>
        <span v-if="entry.cache_write_tokens > 0" class="text-violet">
          {{ t('logs.cacheWriteShort') }} {{ formatTokensCount(entry.cache_write_tokens) }}
        </span>
      </div>
      <span v-else class="text-fg-muted">-</span>
    </TableCell>

    <TableCell v-if="visible.cacheHit" class="font-mono text-xs">
      <span
        v-if="hasCache && cacheHitRatio !== null"
        class="badge badge-info inline-block w-fit font-mono"
        :title="t('logs.cacheHit')"
        data-testid="log-cache-hit"
      >
        ⚡ {{ cacheHitRatio }}%
      </span>
      <span v-else class="text-fg-muted">-</span>
    </TableCell>

    <TableCell v-if="visible.cost" class="font-mono text-xs whitespace-nowrap">
      <span class="font-medium" data-testid="log-cost">{{
        formatUsdMicros(entry.cost_usd_micros)
      }}</span>
    </TableCell>

    <TableCell v-if="visible.settled" class="font-mono text-xs whitespace-nowrap">
      <span v-if="entry.settled" class="text-success font-medium">
        {{ t('logs.settledYes') }}
      </span>
      <span
        v-else
        class="text-warn font-medium"
        data-testid="log-unsettled"
        :title="t('logs.unsettled')"
      >
        {{ t('logs.settledNo') }}
      </span>
    </TableCell>

    <TableCell align="center">
      <button
        type="button"
        class="btn btn-ghost btn-icon"
        data-testid="log-expand"
        :aria-label="t('logs.viewBilling')"
        :title="t('logs.viewBilling')"
        @click.stop="emit('openBilling', $event, entry)"
      >
        <UiIcon name="external-link" :size="15" />
      </button>
    </TableCell>

    <TableCell v-if="visible.body" align="center">
      <button
        type="button"
        class="btn btn-ghost btn-icon"
        data-testid="log-open-body"
        :aria-label="t('logs.viewBody')"
        :title="t('logs.viewBody')"
        @click.stop="emit('openBody', $event, entry)"
      >
        <UiIcon name="code" :size="15" />
      </button>
    </TableCell>
  </TableRow>
</template>
