<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import type { SystemLogEntry } from '@/api/types';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import { formatUnixMillis } from '@/lib/format';
import type { FloatingWindowAnchor } from '@/lib/window-anchor';

const props = withDefaults(
  defineProps<{
    entry: SystemLogEntry;
    anchor?: FloatingWindowAnchor | null;
    stackOrder?: number;
    cascade?: number;
    attention?: boolean;
    topmost?: boolean;
  }>(),
  {
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
  filterTarget: [target: string];
  filterLevel: [level: string];
}>();

const { t, locale } = useI18n();

const copied = ref(false);

const windowTitle = computed(
  () => `#sys_${props.entry.id} · ${props.entry.level.toUpperCase()} · ${props.entry.target}`,
);

function levelBadgeClass(level: string): string {
  switch (level.toLowerCase()) {
    case 'error':
      return 'badge-danger font-bold uppercase';
    case 'warn':
      return 'badge-warn font-semibold uppercase';
    case 'info':
      return 'badge-info uppercase';
    default:
      return 'uppercase';
  }
}

function copyMessage() {
  void navigator.clipboard.writeText(props.entry.message).then(() => {
    copied.value = true;
    window.setTimeout(() => {
      copied.value = false;
    }, 2000);
  });
}
</script>

<template>
  <FloatingWindow
    :title="windowTitle"
    wide
    :anchor="anchor"
    :stack-order="stackOrder"
    :cascade="cascade"
    :attention="attention"
    :topmost="topmost"
    data-testid="system-log-detail-window"
    @close="emit('close')"
    @pointerdown="emit('raise')"
  >
    <template #header-extra>
      <div class="mr-2 ml-auto flex items-center gap-1.5">
        <span class="badge text-[10px]" :class="levelBadgeClass(entry.level)">
          {{ entry.level }}
        </span>
        <button
          type="button"
          class="btn btn-ghost btn-sm gap-1"
          :title="t('logs.copyMessage')"
          @click="copyMessage"
        >
          <UiIcon :name="copied ? 'check' : 'copy'" :size="14" />
          <span class="text-xs">{{
            copied ? t('logs.copiedMessage') : t('logs.copyMessage')
          }}</span>
        </button>
      </div>
    </template>

    <div class="flex flex-col gap-3 p-4">
      <div
        class="rounded-lg border border-[var(--seed-border)] bg-[var(--seed-surface)] p-3 shadow-xs"
      >
        <dl class="grid grid-cols-2 gap-x-2 gap-y-2 text-xs">
          <div>
            <dt class="text-fg-muted text-[11px]">{{ t('logs.created') }}</dt>
            <dd class="font-mono text-[12px] font-medium text-[var(--seed-fg)]">
              {{ formatUnixMillis(entry.created_at, locale) }}
            </dd>
          </div>
          <div>
            <dt class="text-fg-muted flex items-center gap-1 text-[11px]">
              <span>{{ t('logs.level') }}</span>
              <button
                type="button"
                class="text-[var(--fg-muted)] hover:text-[var(--seed-primary)]"
                :title="t('logs.levelFilter')"
                @click="emit('filterLevel', entry.level)"
              >
                <UiIcon name="filter" :size="11" />
              </button>
            </dt>
            <dd class="font-mono font-semibold">
              <span class="badge text-[10px]" :class="levelBadgeClass(entry.level)">
                {{ entry.level }}
              </span>
            </dd>
          </div>
          <div class="col-span-2">
            <dt class="text-fg-muted flex items-center gap-1 text-[11px]">
              <span>{{ t('logs.target') }}</span>
              <button
                type="button"
                class="text-[var(--fg-muted)] hover:text-[var(--seed-primary)]"
                :title="t('logs.targetFilter')"
                @click="emit('filterTarget', entry.target)"
              >
                <UiIcon name="filter" :size="11" />
              </button>
            </dt>
            <dd class="mt-0.5 font-mono text-xs">
              <span class="code-chip rounded px-1.5 py-0.5">{{ entry.target }}</span>
            </dd>
          </div>
        </dl>
      </div>

      <div
        class="rounded-lg border border-[var(--seed-border)] bg-[var(--seed-surface)] p-3 shadow-xs"
      >
        <div class="mb-2 flex items-center justify-between gap-2">
          <span class="text-xs font-semibold tracking-wider text-[var(--fg-muted)] uppercase">
            {{ t('logs.systemMessageFull') }}
          </span>
        </div>
        <pre
          class="seed-scrollbar max-h-96 overflow-auto rounded border border-[var(--seed-border)] bg-[var(--seed-surface-alt)] p-3 font-mono text-xs leading-relaxed break-all whitespace-pre-wrap text-[var(--seed-fg)] select-text"
          >{{ entry.message }}</pre>
      </div>
    </div>
  </FloatingWindow>
</template>
