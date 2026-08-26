<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import type { SystemLogEntry } from '@/api/types';
import FloatingWindow from '@/components/ui/FloatingWindow.vue';
import UiIcon from '@/components/ui/UiIcon.vue';
import { localizedSystemLogMessage } from '@/features/logs/systemLogMessage';
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

const i18n = useI18n();
const { locale } = i18n;
const t = (key: string, values?: Record<string, unknown>) => i18n.t(key, values ?? {});
const te = (key: string) => i18n.te(key);

const copied = ref(false);

const windowTitle = computed(
  () => `#sys_${props.entry.id} · ${props.entry.level.toUpperCase()} · ${props.entry.target}`,
);
const displayMessage = computed(() => {
  void locale.value;
  return localizedSystemLogMessage(props.entry, t, te);
});

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

function levelLabel(level: string): string {
  return t(`logs.levels.${level.toLowerCase()}`);
}

function copyMessage() {
  void navigator.clipboard.writeText(displayMessage.value).then(() => {
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
          {{ levelLabel(entry.level) }}
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

    <div class="flex flex-col gap-4 p-5">
      <!-- 基础元数据（通透无多重卡片边框） -->
      <div class="flex flex-col gap-2.5">
        <div class="flex items-center justify-between pb-1">
          <span class="text-xs font-bold tracking-wider text-[var(--fg-muted)] uppercase">
            {{ t('logs.basicInfo') }}
          </span>
          <span class="font-mono text-xs text-[var(--fg-muted)]">
            {{ formatUnixMillis(entry.created_at, locale) }}
          </span>
        </div>

        <dl class="grid grid-cols-2 gap-x-6 gap-y-3 text-xs sm:grid-cols-3">
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
            <dd class="mt-1 font-mono font-semibold">
              <span class="badge text-[10px]" :class="levelBadgeClass(entry.level)">
                {{ levelLabel(entry.level) }}
              </span>
            </dd>
          </div>

          <div>
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
            <dd class="mt-1 font-mono text-xs">
              <span class="code-chip rounded px-1.5 py-0.5">{{ entry.target }}</span>
            </dd>
          </div>

          <!-- 运维事件由系统自身产生，没有操作者；审计事件才有。 -->
          <div class="col-span-2 sm:col-span-1">
            <dt class="text-fg-muted text-[11px]">{{ t('logs.actor') }}</dt>
            <dd class="mt-1 font-mono text-xs" data-testid="system-log-detail-actor">
              <span v-if="entry.actor_email">
                {{ entry.actor_email }}
                <span class="text-fg-subtle">(#{{ entry.actor_user_id }})</span>
              </span>
              <span v-else class="text-fg-subtle">{{ t('logs.actorSystem') }}</span>
            </dd>
          </div>
        </dl>
      </div>

      <div class="h-px bg-[var(--seed-border)]/60" />

      <!-- 日志消息与堆栈 -->
      <div class="flex flex-col gap-2">
        <div class="flex items-center justify-between">
          <span class="text-xs font-bold tracking-wider text-[var(--fg-muted)] uppercase">
            {{ t('logs.systemMessageFull') }}
          </span>
        </div>
        <pre
          class="seed-scrollbar max-h-96 overflow-auto rounded-md border border-[var(--seed-border)] bg-[var(--seed-surface-alt)] p-3 font-mono text-xs leading-relaxed break-all whitespace-pre-wrap text-[var(--seed-fg)] select-text"
          >{{ displayMessage }}</pre>
      </div>
    </div>
  </FloatingWindow>
</template>
