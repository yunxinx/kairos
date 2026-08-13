<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import type { LogEntry } from '@/api/types';
import { decodeLogBody, type DecodedLogBody } from '@/lib/log-body';

const props = defineProps<{
  entry: LogEntry;
}>();

const { t } = useI18n();

const copiedSide = ref<'request' | 'response' | null>(null);

const requestBody = computed(() => decodeLogBody(props.entry.request_body));
const responseBody = computed(() => decodeLogBody(props.entry.response_body));

function copyText(side: 'request' | 'response', decoded: DecodedLogBody) {
  if (decoded.kind !== 'json' && decoded.kind !== 'text') {
    return;
  }
  void navigator.clipboard.writeText(decoded.text).then(
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

function bodyTestId(side: 'request' | 'response', decoded: DecodedLogBody): string {
  if (decoded.kind === 'binary') {
    return `log-${side}-body-binary`;
  }
  return `log-${side}-body`;
}
</script>

<template>
  <div class="grid gap-4 lg:grid-cols-2" data-testid="log-body-panel">
    <section>
      <div class="mb-2 flex items-center justify-between gap-2">
        <h3 class="text-sm font-medium">{{ t('logs.requestBody') }}</h3>
        <button
          v-if="requestBody.kind === 'json' || requestBody.kind === 'text'"
          type="button"
          class="btn btn-sm btn-subtle"
          data-testid="log-request-copy"
          @click="copyText('request', requestBody)"
        >
          {{ copiedSide === 'request' ? t('common.copied') : t('common.copy') }}
        </button>
      </div>
      <p
        v-if="requestBody.kind === 'empty'"
        class="text-fg-muted text-sm"
        :data-testid="bodyTestId('request', requestBody)"
      >
        {{ t('logs.bodyEmpty') }}
      </p>
      <p
        v-else-if="requestBody.kind === 'binary'"
        class="text-danger text-sm"
        :data-testid="bodyTestId('request', requestBody)"
      >
        {{ t('logs.bodyBinary', { bytes: requestBody.byteLength }) }}
      </p>
      <pre
        v-else
        class="seed-scrollbar text-fg max-h-64 overflow-auto rounded-md bg-[var(--seed-surface-alt)] p-3 font-mono text-xs break-all whitespace-pre-wrap"
        :data-testid="bodyTestId('request', requestBody)"
        >{{ requestBody.text }}</pre>
    </section>
    <section>
      <div class="mb-2 flex items-center justify-between gap-2">
        <h3 class="text-sm font-medium">{{ t('logs.responseBody') }}</h3>
        <button
          v-if="responseBody.kind === 'json' || responseBody.kind === 'text'"
          type="button"
          class="btn btn-sm btn-subtle"
          data-testid="log-response-copy"
          @click="copyText('response', responseBody)"
        >
          {{ copiedSide === 'response' ? t('common.copied') : t('common.copy') }}
        </button>
      </div>
      <p
        v-if="responseBody.kind === 'empty'"
        class="text-fg-muted text-sm"
        :data-testid="bodyTestId('response', responseBody)"
      >
        {{ t('logs.bodyEmpty') }}
      </p>
      <p
        v-else-if="responseBody.kind === 'binary'"
        class="text-danger text-sm"
        :data-testid="bodyTestId('response', responseBody)"
      >
        {{ t('logs.bodyBinary', { bytes: responseBody.byteLength }) }}
      </p>
      <pre
        v-else
        class="seed-scrollbar text-fg max-h-64 overflow-auto rounded-md bg-[var(--seed-surface-alt)] p-3 font-mono text-xs break-all whitespace-pre-wrap"
        :data-testid="bodyTestId('response', responseBody)"
        >{{ responseBody.text }}</pre>
    </section>
  </div>
</template>
