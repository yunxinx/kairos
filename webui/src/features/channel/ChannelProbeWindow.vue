<script setup lang="ts">
// 渠道连通性探测浮窗：按清单去重列出可测模型；可勾选后批量测，也可逐行测。
// 出站一律用主模型名。成功/失败/超时都弹出详情浮窗（3s 自动消失，悬浮暂停）。
import { computed, onUnmounted, ref, useId } from 'vue';
import { useI18n } from 'vue-i18n';
import { apiClient, extractApiError } from '@/api/client';
import { channelOutboundUrl, type ChannelProbeResult, type ChannelView } from '@/api/types';
import Checkbox from '@/components/ui/Checkbox.vue';
import DataTablePanel from '@/components/ui/DataTablePanel.vue';
import EmptyState from '@/components/ui/EmptyState.vue';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import SearchInput from '@/components/ui/SearchInput.vue';
import SelectCell from '@/components/ui/data-table/SelectCell.vue';
import Table from '@/components/ui/table/Table.vue';
import TableBody from '@/components/ui/table/TableBody.vue';
import TableCell from '@/components/ui/table/TableCell.vue';
import TableHead from '@/components/ui/table/TableHead.vue';
import TableHeader from '@/components/ui/table/TableHeader.vue';
import TableRow from '@/components/ui/table/TableRow.vue';
import { formatProbeLatency } from '@/lib/format';
import { probeModelRows } from '@/lib/model-list';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

/** 详情浮窗自动消失时长；鼠标悬浮/键盘焦点暂停计时。 */
const RESULT_VISIBLE_MS = 3_000;

type ProbeOutcome = 'idle' | 'testing' | 'success' | 'failure' | 'timeout' | 'unreachable';

const OUTCOME_BADGE: Record<ProbeOutcome, string> = {
  idle: 'badge-neutral',
  testing: 'badge-info',
  success: 'badge-success',
  failure: 'badge-danger',
  timeout: 'badge-warn',
  unreachable: 'badge-danger',
};

const OUTCOME_KEY: Record<ProbeOutcome, string> = {
  idle: 'channel.probeStatusIdle',
  testing: 'channel.probeStatusTesting',
  success: 'channel.probeStatusSuccess',
  failure: 'channel.probeStatusFailure',
  timeout: 'channel.probeStatusTimeout',
  unreachable: 'channel.probeStatusUnreachable',
};

const RESULT_TITLE_KEY: Record<Exclude<ProbeOutcome, 'idle' | 'testing'>, string> = {
  success: 'channel.probeResultSuccessTitle',
  failure: 'channel.probeResultFailureTitle',
  timeout: 'channel.probeResultTimeoutTitle',
  unreachable: 'channel.probeResultUnreachableTitle',
};

interface ResultPopup {
  outcome: Exclude<ProbeOutcome, 'idle' | 'testing'>;
  message: string;
  model: string;
  result: ChannelProbeResult | null;
}

const props = withDefaults(
  defineProps<{
    channel: ChannelView;
    anchor?: FloatingWindowAnchor | null;
    stackOrder?: number;
    cascade?: number;
    attention?: boolean;
    topmost?: boolean;
  }>(),
  { anchor: null, stackOrder: 0, cascade: 0, attention: false, topmost: true },
);

const emit = defineEmits<{
  close: [];
  raise: [];
}>();

const { t } = useI18n();
const probeSearchId = `channel-probe-search-${useId()}`;

const rows = computed(() => probeModelRows(props.channel.models, props.channel.model_aliases));
const probeSearch = ref('');

const filteredRows = computed(() => {
  const query = probeSearch.value.trim().toLowerCase();
  if (query === '') return rows.value;
  return rows.value.filter(
    (row) =>
      row.displayName.toLowerCase().includes(query) || row.probeModel.toLowerCase().includes(query),
  );
});

const emptyTitle = computed(() =>
  rows.value.length === 0 ? t('channel.probeEmpty') : t('channel.probeEmptySearch'),
);
const probeTargetUrl = computed(() =>
  channelOutboundUrl(props.channel.protocol, props.channel.base_url),
);
const selection = ref<Set<string>>(new Set());
const results = ref<Record<string, ChannelProbeResult>>({});
/** 管理面/网络错误没有探测结果，单独记下以免弹窗后行回到「未测试」。 */
const clientUnreachable = ref<Set<string>>(new Set());
const testingModel = ref<string | null>(null);

const resultPopup = ref<ResultPopup | null>(null);
const resultAnchor = ref<FloatingWindowAnchor | null>(null);
const testSelectedBtn = ref<HTMLElement | null>(null);

let dismissTimer: ReturnType<typeof setTimeout> | undefined;
let dismissRemaining = RESULT_VISIBLE_MS;
let dismissStarted = 0;

function outcomeFromResult(result: ChannelProbeResult): Exclude<ProbeOutcome, 'idle' | 'testing'> {
  if (result.timed_out) return 'timeout';
  if (!result.reachable) return 'unreachable';
  if (result.error) return 'failure';
  return 'success';
}

function outcomeOf(probeModel: string): ProbeOutcome {
  if (testingModel.value === probeModel) return 'testing';
  const result = results.value[probeModel];
  if (result) return outcomeFromResult(result);
  if (clientUnreachable.value.has(probeModel)) return 'unreachable';
  return 'idle';
}

function showPopup(popup: ResultPopup, buttonEl: HTMLElement | null) {
  const rect = buttonEl?.getBoundingClientRect();
  resultAnchor.value = rect ? { x: rect.left, y: rect.bottom } : null;
  resultPopup.value = popup;
  dismissRemaining = RESULT_VISIBLE_MS;
  startDismiss();
}

function dismissPopup() {
  if (dismissTimer !== undefined) {
    clearTimeout(dismissTimer);
    dismissTimer = undefined;
  }
  resultPopup.value = null;
}

function startDismiss() {
  if (dismissTimer !== undefined) clearTimeout(dismissTimer);
  dismissStarted = Date.now();
  dismissTimer = setTimeout(
    () => {
      dismissTimer = undefined;
      resultPopup.value = null;
    },
    Math.max(dismissRemaining, 0),
  );
}

function pauseDismiss() {
  if (dismissTimer === undefined) return;
  clearTimeout(dismissTimer);
  dismissTimer = undefined;
  dismissRemaining -= Date.now() - dismissStarted;
}

function resumeDismiss() {
  if (resultPopup.value === null || dismissTimer !== undefined) return;
  startDismiss();
}

onUnmounted(() => {
  if (dismissTimer !== undefined) clearTimeout(dismissTimer);
});

function toggleRow(probeModel: string) {
  const next = new Set(selection.value);
  if (next.has(probeModel)) {
    next.delete(probeModel);
  } else {
    next.add(probeModel);
  }
  selection.value = next;
}

const allVisibleSelected = computed(
  () =>
    filteredRows.value.length > 0 &&
    filteredRows.value.every((row) => selection.value.has(row.probeModel)),
);
const someVisibleSelected = computed(() =>
  filteredRows.value.some((row) => selection.value.has(row.probeModel)),
);

function toggleAllVisible() {
  const next = new Set(selection.value);
  if (allVisibleSelected.value) {
    for (const row of filteredRows.value) next.delete(row.probeModel);
  } else {
    for (const row of filteredRows.value) next.add(row.probeModel);
  }
  selection.value = next;
}

const busy = computed(() => testingModel.value !== null);
const canTestSelected = computed(() => selection.value.size > 0 && !busy.value);

function markClientUnreachable(probeModel: string) {
  results.value = Object.fromEntries(
    Object.entries(results.value).filter(([model]) => model !== probeModel),
  );
  const next = new Set(clientUnreachable.value);
  next.add(probeModel);
  clientUnreachable.value = next;
}

function clearClientUnreachable(probeModel: string) {
  if (!clientUnreachable.value.has(probeModel)) return;
  const next = new Set(clientUnreachable.value);
  next.delete(probeModel);
  clientUnreachable.value = next;
}

async function testOne(probeModel: string, buttonEl: HTMLElement | null) {
  if (busy.value) return;
  testingModel.value = probeModel;
  try {
    const result = await apiClient.testChannel(props.channel.id, probeModel);
    clearClientUnreachable(probeModel);
    results.value = { ...results.value, [probeModel]: result };
    const outcome = outcomeFromResult(result);
    showPopup(
      {
        outcome,
        message: result.error ?? t('channel.probeStatusSuccess'),
        model: probeModel,
        result,
      },
      buttonEl,
    );
  } catch (err) {
    markClientUnreachable(probeModel);
    showPopup(
      {
        outcome: 'unreachable',
        message: extractApiError(err).message,
        model: probeModel,
        result: null,
      },
      buttonEl,
    );
  } finally {
    testingModel.value = null;
  }
}

async function testSelected(event: Event) {
  const buttonEl = (event.currentTarget as HTMLElement | null) ?? testSelectedBtn.value;
  const targets = rows.value
    .filter((row) => selection.value.has(row.probeModel))
    .map((row) => row.probeModel);
  for (const probeModel of targets) {
    await testOne(probeModel, buttonEl);
  }
}

function runRow(probeModel: string, event: Event) {
  void testOne(probeModel, event.currentTarget as HTMLElement);
}
</script>

<template>
  <FloatingWindow
    wide
    :title="t('channel.probeTitle', { name: channel.name })"
    :anchor="anchor"
    :stack-order="stackOrder"
    :cascade="cascade"
    :attention="attention"
    :topmost="topmost"
    @close="emit('close')"
    @pointerdown="emit('raise')"
  >
    <div class="card-body space-y-3" data-testid="channel-probe-view">
      <div class="flex items-center gap-2">
        <SearchInput
          :id="probeSearchId"
          v-model="probeSearch"
          class="search-input-sm max-w-sm min-w-0"
          :placeholder="t('channel.probeSearchPlaceholder')"
          data-testid="channel-probe-search"
        />
        <button
          ref="testSelectedBtn"
          type="button"
          class="btn btn-sm ml-auto shrink-0"
          data-testid="channel-probe-test-selected"
          :disabled="!canTestSelected"
          @click="(event) => void testSelected(event)"
        >
          {{ t('channel.probeTestSelected') }}
        </button>
      </div>
      <DataTablePanel>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead class="w-10">
                <div class="flex items-center justify-center">
                  <Checkbox
                    :model-value="allVisibleSelected"
                    :indeterminate="someVisibleSelected && !allVisibleSelected"
                    data-testid="channel-probe-select-all"
                    :aria-label="t('common.selectAll')"
                    @update:model-value="toggleAllVisible"
                  />
                </div>
              </TableHead>
              <TableHead>{{ t('channel.probeColModel') }}</TableHead>
              <TableHead class="w-24">{{ t('channel.probeColStatus') }}</TableHead>
              <TableHead class="w-24">{{ t('channel.probeColLatency') }}</TableHead>
              <TableHead class="w-24" align="right">{{ t('channel.probeColAction') }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow
              v-for="row in filteredRows"
              :key="row.probeModel"
              :data-state="selection.has(row.probeModel) ? 'selected' : undefined"
              data-testid="channel-probe-row"
              :data-model="row.displayName"
              class="cursor-pointer"
              @click="toggleRow(row.probeModel)"
            >
              <SelectCell
                :checked="selection.has(row.probeModel)"
                test-id="channel-probe-checkbox"
                @toggle="toggleRow(row.probeModel)"
                @click.stop
              />
              <TableCell class="font-mono text-xs">{{ row.displayName }}</TableCell>
              <TableCell>
                <span
                  class="badge"
                  :class="OUTCOME_BADGE[outcomeOf(row.probeModel)]"
                  :data-testid="`channel-probe-status-${outcomeOf(row.probeModel)}`"
                >
                  {{ t(OUTCOME_KEY[outcomeOf(row.probeModel)]) }}
                </span>
              </TableCell>
              <TableCell class="font-mono text-xs" data-testid="channel-probe-latency">
                {{
                  results[row.probeModel]
                    ? formatProbeLatency(results[row.probeModel]!.latency_ms)
                    : '—'
                }}
              </TableCell>
              <TableCell align="right" @click.stop>
                <button
                  type="button"
                  class="btn btn-sm"
                  data-testid="channel-probe-run"
                  :disabled="busy"
                  @click="runRow(row.probeModel, $event)"
                >
                  {{ testingModel === row.probeModel ? t('channel.testing') : t('channel.test') }}
                </button>
              </TableCell>
            </TableRow>
            <TableRow v-if="filteredRows.length === 0">
              <TableCell :colspan="5" class="h-24 whitespace-normal">
                <EmptyState :title="emptyTitle" />
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>
      </DataTablePanel>
    </div>
  </FloatingWindow>

  <FloatingWindow
    v-if="resultPopup"
    :title="t(RESULT_TITLE_KEY[resultPopup.outcome])"
    :anchor="resultAnchor"
    :stack-order="stackOrder + 1"
    :topmost="false"
    @close="dismissPopup"
    @mouseenter="pauseDismiss"
    @mouseleave="resumeDismiss"
    @focusin="pauseDismiss"
    @focusout="resumeDismiss"
  >
    <div class="card-body space-y-2" data-testid="channel-probe-detail">
      <p
        class="text-sm"
        :class="resultPopup.outcome === 'success' ? 'text-success' : 'text-danger'"
      >
        {{ resultPopup.message }}
      </p>
      <div class="text-fg-muted space-y-1 text-xs">
        <p class="font-medium">{{ t('channel.probeResultDetail') }}</p>
        <p class="font-mono">{{ t('channel.probeResultModel') }}: {{ resultPopup.model }}</p>
        <p v-if="resultPopup.result?.status_code != null" class="font-mono">
          {{ t('channel.probeResultStatus') }}: {{ resultPopup.result.status_code }}
        </p>
        <p v-if="resultPopup.result" class="font-mono">
          {{ t('channel.probeResultLatency') }}:
          {{ formatProbeLatency(resultPopup.result.latency_ms) }}
        </p>
        <p class="font-mono break-all">
          {{ t('channel.probeResultTarget') }}: {{ probeTargetUrl }}
        </p>
        <p v-if="resultPopup.result?.upstream_body" class="font-mono break-all whitespace-pre-wrap">
          {{ t('channel.probeResultBody') }}: {{ resultPopup.result.upstream_body }}
        </p>
      </div>
    </div>
  </FloatingWindow>
</template>
