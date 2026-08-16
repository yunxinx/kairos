<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import type { LogEntry } from '@/api/types';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import LogBodyPanel from '@/features/logs/LogBodyPanel.vue';
import { formatUnixMillis, formatUsdMicros } from '@/lib/format';

defineProps<{
  entry: LogEntry;
  expanded: boolean;
  detailColSpan: number;
}>();

const emit = defineEmits<{
  toggleExpand: [];
}>();

const { t, locale } = useI18n();

function statusBadgeClass(statusCode: number): string {
  return statusCode >= 200 && statusCode < 300 ? 'badge-success' : 'badge-danger';
}
</script>

<template>
  <TableRow
    data-testid="log-row"
    :data-log-id="String(entry.id)"
    :data-model="entry.model"
    :data-token-key="entry.token_key"
  >
    <TableCell class="text-fg-muted font-mono text-xs">
      {{ formatUnixMillis(entry.created_at, locale) }}
    </TableCell>
    <TableCell class="max-w-0 truncate" :title="entry.token_key">
      {{ entry.token_name }}
    </TableCell>
    <TableCell data-testid="log-model">{{ entry.model }}</TableCell>
    <TableCell data-testid="log-channel">{{ entry.channel }}</TableCell>
    <TableCell>
      <span class="badge" :class="statusBadgeClass(entry.status_code)" data-testid="log-status">{{
        entry.status_code
      }}</span>
    </TableCell>
    <TableCell class="font-mono text-sm" data-testid="log-latency">
      {{ entry.latency_ms }} ms
    </TableCell>
    <TableCell class="font-mono text-sm" data-testid="log-cost">
      {{ formatUsdMicros(entry.cost_usd_micros) }}
    </TableCell>
    <TableCell align="right" class="w-10">
      <button
        type="button"
        class="btn btn-ghost btn-icon"
        data-testid="log-expand"
        :aria-expanded="expanded ? 'true' : 'false'"
        :aria-label="expanded ? t('logs.collapseDetails') : t('logs.expandDetails')"
        @click="emit('toggleExpand')"
      >
        <UiIcon :name="expanded ? 'chevron-up' : 'chevron-down'" :size="16" />
      </button>
    </TableCell>
  </TableRow>
  <TableRow v-if="expanded" data-testid="log-detail-row" class="hover:bg-transparent">
    <TableCell :colspan="detailColSpan" class="align-top whitespace-normal">
      <dl class="mb-4 grid gap-3 sm:grid-cols-2" data-testid="log-detail-meta">
        <div>
          <dt class="text-fg-muted text-xs">{{ t('logs.outboundModel') }}</dt>
          <dd class="font-mono text-sm" data-testid="log-outbound-model">
            {{ entry.outbound_model ?? entry.model }}
          </dd>
        </div>
        <div>
          <dt class="text-fg-muted text-xs">{{ t('logs.channel') }}</dt>
          <dd class="font-mono text-sm" data-testid="log-detail-channel">
            {{ entry.channel }}
          </dd>
        </div>
      </dl>
      <LogBodyPanel :entry="entry" />
    </TableCell>
  </TableRow>
</template>
