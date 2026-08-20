<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import type { LogEntry } from '@/api/types';
import UiIcon from '@/components/ui/UiIcon.vue';
import SegmentSwitch, { type SegmentPair } from '@/components/ui/SegmentSwitch.vue';
import { decodeLogBody, type DecodedLogBody } from '@/lib/log-body';
import { parseChatInspection, generateCurlCommand, formatJsonArgs } from '@/lib/log-messages';

const props = defineProps<{
  entry: LogEntry;
}>();

const { t } = useI18n();

const copiedSide = ref<'request' | 'response' | 'curl' | null>(null);
const showTools = ref(false);
const showThinking = ref(true);

const requestBody = computed(() => decodeLogBody(props.entry.request_body));
const responseBody = computed(() => decodeLogBody(props.entry.response_body));

const rawRequestText = computed(() =>
  requestBody.value.kind === 'json' || requestBody.value.kind === 'text'
    ? requestBody.value.text
    : null,
);

const rawResponseText = computed(() =>
  responseBody.value.kind === 'json' || responseBody.value.kind === 'text'
    ? responseBody.value.text
    : null,
);

const inspection = computed(() => parseChatInspection(rawRequestText.value, rawResponseText.value));

type ViewMode = 'visual' | 'raw';
const viewMode = ref<ViewMode>(inspection.value.isChat ? 'visual' : 'raw');

watch(
  () => inspection.value.isChat,
  (isChat) => {
    if (isChat) {
      viewMode.value = 'visual';
    } else {
      viewMode.value = 'raw';
    }
  },
);

const modeOptions = computed<SegmentPair<ViewMode>>(() => [
  { value: 'visual', label: t('logs.viewModeVisual'), testId: 'log-view-visual' },
  { value: 'raw', label: t('logs.viewModeRaw'), testId: 'log-view-raw' },
]);

function copyText(side: 'request' | 'response' | 'curl', text: string) {
  void navigator.clipboard.writeText(text).then(
    () => {
      copiedSide.value = side;
      window.setTimeout(() => {
        if (copiedSide.value === side) {
          copiedSide.value = null;
        }
      }, 2000);
    },
    () => {
      copiedSide.value = null;
    },
  );
}

function copyCurl() {
  const curl = generateCurlCommand(props.entry, rawRequestText.value);
  copyText('curl', curl);
}

function bodyTestId(side: 'request' | 'response', decoded: DecodedLogBody): string {
  if (decoded.kind === 'binary') {
    return `log-${side}-body-binary`;
  }
  return `log-${side}-body`;
}

function roleBadgeClass(role: string): string {
  switch (role.toLowerCase()) {
    case 'system':
      return 'bg-amber-500/15 text-amber-600 dark:text-amber-400 border border-amber-500/30';
    case 'user':
      return 'bg-blue-500/15 text-blue-600 dark:text-blue-400 border border-blue-500/30';
    case 'assistant':
      return 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-400 border border-emerald-500/30';
    case 'tool':
      return 'bg-purple-500/15 text-purple-600 dark:text-purple-400 border border-purple-500/30';
    default:
      return 'bg-slate-500/15 text-slate-600 dark:text-slate-400 border border-slate-500/30';
  }
}
</script>

<template>
  <div class="flex flex-col gap-3" data-testid="log-body-panel">
    <div
      class="flex flex-wrap items-center justify-between gap-2 border-b border-[var(--seed-border)] pb-3"
    >
      <div class="flex items-center gap-2">
        <SegmentSwitch
          v-if="inspection.isChat"
          v-model="viewMode"
          :options="modeOptions"
          :aria-label="t('logs.kinds')"
        />
        <span v-else class="text-xs font-semibold text-[var(--fg-muted)]">
          {{ t('logs.viewModeRaw') }}
        </span>
      </div>

      <div class="flex flex-wrap items-center gap-2">
        <button
          type="button"
          class="btn btn-sm btn-subtle gap-1.5"
          data-testid="log-copy-curl"
          :title="t('logs.copyCurl')"
          @click="copyCurl"
        >
          <UiIcon :name="copiedSide === 'curl' ? 'check' : 'terminal'" :size="14" />
          <span>{{ copiedSide === 'curl' ? t('logs.copiedCurl') : t('logs.copyCurl') }}</span>
        </button>

        <button
          v-if="rawRequestText"
          type="button"
          class="btn btn-sm btn-subtle gap-1.5"
          data-testid="log-request-copy"
          @click="copyText('request', rawRequestText)"
        >
          <UiIcon :name="copiedSide === 'request' ? 'check' : 'copy'" :size="14" />
          <span>{{ copiedSide === 'request' ? t('common.copied') : t('logs.requestBody') }}</span>
        </button>

        <button
          v-if="rawResponseText"
          type="button"
          class="btn btn-sm btn-subtle gap-1.5"
          data-testid="log-response-copy"
          @click="copyText('response', rawResponseText)"
        >
          <UiIcon :name="copiedSide === 'response' ? 'check' : 'copy'" :size="14" />
          <span>{{ copiedSide === 'response' ? t('common.copied') : t('logs.responseBody') }}</span>
        </button>
      </div>
    </div>

    <div v-if="viewMode === 'visual' && inspection.isChat" class="flex flex-col gap-3 py-1">
      <div
        v-if="inspection.systemPrompt"
        class="rounded-md border border-amber-500/30 bg-amber-500/5 p-3 text-xs leading-relaxed"
      >
        <div
          class="mb-1.5 flex items-center gap-1.5 font-semibold text-amber-600 dark:text-amber-400"
        >
          <span
            class="rounded bg-amber-500/20 px-1.5 py-0.5 font-mono text-[10px] tracking-wide uppercase"
          >
            System
          </span>
          <span>{{ t('logs.systemPrompt') }}</span>
        </div>
        <p class="font-mono break-words whitespace-pre-wrap text-[var(--seed-fg)] opacity-90">
          {{ inspection.systemPrompt }}
        </p>
      </div>

      <div
        v-if="inspection.tools.length > 0"
        class="rounded-md border border-[var(--seed-border)] bg-[var(--seed-surface)] p-2.5"
      >
        <button
          type="button"
          class="flex w-full items-center justify-between text-left text-xs font-semibold text-[var(--fg-muted)] hover:text-[var(--seed-fg)]"
          @click="showTools = !showTools"
        >
          <span class="flex items-center gap-1.5">
            <UiIcon name="code" :size="14" />
            {{ t('logs.declaredTools', { count: inspection.tools.length }) }}
          </span>
          <UiIcon :name="showTools ? 'chevron-up' : 'chevron-down'" :size="14" />
        </button>
        <div v-if="showTools" class="mt-2.5 grid gap-2 sm:grid-cols-2">
          <div
            v-for="tool in inspection.tools"
            :key="tool.name"
            class="rounded border border-[var(--seed-border)] bg-[var(--seed-surface-alt)] p-2 font-mono text-xs"
          >
            <div class="font-bold text-[var(--seed-primary)]">{{ tool.name }}</div>
            <div v-if="tool.description" class="text-fg-muted mt-0.5 text-[11px]">
              {{ tool.description }}
            </div>
          </div>
        </div>
      </div>

      <div class="flex flex-col gap-2.5">
        <div
          v-for="(msg, index) in inspection.messages"
          :key="index"
          class="rounded-lg border border-[var(--seed-border)] bg-[var(--seed-surface)] p-3 shadow-xs"
        >
          <div class="mb-2 flex items-center justify-between gap-2">
            <span
              class="rounded px-2 py-0.5 font-mono text-[10px] font-bold tracking-wider uppercase"
              :class="roleBadgeClass(msg.role)"
            >
              {{ msg.role }} {{ msg.name ? `(${msg.name})` : '' }}
            </span>
            <span v-if="msg.toolUseId" class="font-mono text-[10px] text-[var(--fg-muted)]">
              call_id: {{ msg.toolUseId }}
            </span>
          </div>

          <div v-if="msg.toolCalls && msg.toolCalls.length > 0" class="mb-2 flex flex-col gap-2">
            <div
              v-for="(tc, tcIdx) in msg.toolCalls"
              :key="tcIdx"
              class="rounded border border-purple-500/30 bg-purple-500/5 p-2 font-mono text-xs"
            >
              <div
                class="flex items-center gap-1 font-semibold text-purple-600 dark:text-purple-400"
              >
                <UiIcon name="code" :size="13" />
                <span>{{ tc.name }}</span>
              </div>
              <pre
                class="mt-1 max-h-40 overflow-auto text-[11px] whitespace-pre-wrap text-[var(--seed-fg)]"
                >{{ formatJsonArgs(tc.arguments) }}</pre>
            </div>
          </div>

          <div
            v-if="msg.content"
            class="font-mono text-xs leading-relaxed break-words whitespace-pre-wrap text-[var(--seed-fg)]"
          >
            {{ msg.content }}
          </div>
        </div>
      </div>

      <div
        v-if="inspection.response"
        class="rounded-lg border border-emerald-500/40 bg-[var(--seed-surface)] p-3 shadow-xs"
      >
        <div class="mb-2 flex items-center justify-between gap-2">
          <div class="flex items-center gap-2">
            <span
              class="rounded border border-emerald-500/30 bg-emerald-500/15 px-2 py-0.5 font-mono text-[10px] font-bold tracking-wider text-emerald-600 uppercase dark:text-emerald-400"
            >
              Assistant (Response)
            </span>
            <span v-if="inspection.response.isStream" class="badge badge-info text-[10px]">
              SSE Stream
            </span>
          </div>
          <span
            v-if="inspection.response.finishReason"
            class="badge bg-[var(--seed-surface-alt)] font-mono text-[10px] text-[var(--fg-muted)]"
          >
            finish: {{ inspection.response.finishReason }}
          </span>
        </div>

        <div
          v-if="inspection.response.reasoning"
          class="mb-3 rounded border border-blue-500/30 bg-blue-500/5 p-2.5"
        >
          <button
            type="button"
            class="flex w-full items-center justify-between text-left text-xs font-semibold text-blue-600 dark:text-blue-400"
            @click="showThinking = !showThinking"
          >
            <span class="flex items-center gap-1.5">
              <span>💭</span>
              <span>{{ t('logs.thinking') }}</span>
            </span>
            <UiIcon :name="showThinking ? 'chevron-up' : 'chevron-down'" :size="13" />
          </button>
          <pre
            v-if="showThinking"
            class="mt-2 max-h-56 overflow-auto font-mono text-xs leading-relaxed break-words whitespace-pre-wrap text-[var(--seed-fg)] opacity-85"
            >{{ inspection.response.reasoning }}</pre>
        </div>

        <div
          v-if="inspection.response.toolCalls && inspection.response.toolCalls.length > 0"
          class="mb-2 flex flex-col gap-2"
        >
          <div
            v-for="(tc, tcIdx) in inspection.response.toolCalls"
            :key="tcIdx"
            class="rounded border border-purple-500/30 bg-purple-500/5 p-2 font-mono text-xs"
          >
            <div class="flex items-center gap-1 font-semibold text-purple-600 dark:text-purple-400">
              <UiIcon name="code" :size="13" />
              <span>{{ tc.name }}</span>
            </div>
            <pre
              class="mt-1 max-h-40 overflow-auto text-[11px] whitespace-pre-wrap text-[var(--seed-fg)]"
              >{{ tc.arguments }}</pre>
          </div>
        </div>

        <div
          v-if="inspection.response.content"
          class="font-mono text-xs leading-relaxed break-words whitespace-pre-wrap text-[var(--seed-fg)]"
        >
          {{ inspection.response.content }}
        </div>
      </div>
    </div>

    <div v-show="viewMode === 'raw' || !inspection.isChat" class="grid gap-4 lg:grid-cols-2">
      <section class="flex flex-col">
        <div class="mb-2 flex items-center justify-between gap-2">
          <h4 class="text-xs font-semibold tracking-wider text-[var(--fg-muted)] uppercase">
            {{ t('logs.requestBody') }}
          </h4>
        </div>
        <p
          v-if="requestBody.kind === 'empty'"
          class="text-fg-muted text-xs italic"
          :data-testid="bodyTestId('request', requestBody)"
        >
          {{ t('logs.bodyEmpty') }}
        </p>
        <p
          v-else-if="requestBody.kind === 'binary'"
          class="text-danger font-mono text-xs"
          :data-testid="bodyTestId('request', requestBody)"
        >
          {{ t('logs.bodyBinary', { bytes: requestBody.byteLength }) }}
        </p>
        <pre
          v-else
          class="seed-scrollbar text-fg max-h-72 overflow-auto rounded-md border border-[var(--seed-border)] bg-[var(--seed-surface-alt)] p-3 font-mono text-xs break-all whitespace-pre-wrap"
          :data-testid="bodyTestId('request', requestBody)"
          >{{ requestBody.text }}</pre>
      </section>

      <section class="flex flex-col">
        <div class="mb-2 flex items-center justify-between gap-2">
          <h4 class="text-xs font-semibold tracking-wider text-[var(--fg-muted)] uppercase">
            {{ t('logs.responseBody') }}
          </h4>
        </div>
        <p
          v-if="responseBody.kind === 'empty'"
          class="text-fg-muted text-xs italic"
          :data-testid="bodyTestId('response', responseBody)"
        >
          {{ t('logs.bodyEmpty') }}
        </p>
        <p
          v-else-if="responseBody.kind === 'binary'"
          class="text-danger font-mono text-xs"
          :data-testid="bodyTestId('response', responseBody)"
        >
          {{ t('logs.bodyBinary', { bytes: responseBody.byteLength }) }}
        </p>
        <pre
          v-else
          class="seed-scrollbar text-fg max-h-72 overflow-auto rounded-md border border-[var(--seed-border)] bg-[var(--seed-surface-alt)] p-3 font-mono text-xs break-all whitespace-pre-wrap"
          :data-testid="bodyTestId('response', responseBody)"
          >{{ responseBody.text }}</pre>
      </section>
    </div>
  </div>
</template>
