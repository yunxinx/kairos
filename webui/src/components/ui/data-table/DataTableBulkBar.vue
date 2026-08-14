<script setup lang="ts">
// 批量操作浮动条：选中行后视口底部居中浮出；Escape 清空选择。
// 结构对齐 shadcn-admin bulk-actions：清除按钮 → 分隔线 → 计数 → 分隔线 → 操作按钮插槽。
import { onBeforeUnmount, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import UiIcon from '@/components/ui/UiIcon.vue';

defineOptions({ inheritAttrs: false });

const props = defineProps<{
  /** 已选行数；为 0 时整条隐藏。 */
  count: number;
}>();

const emit = defineEmits<{
  clear: [];
}>();

const { t } = useI18n();

function onKeydown(event: KeyboardEvent) {
  if (event.key !== 'Escape') return;
  // 弹层打开时的 Escape 交给弹层自身消费，不误清选择。
  const target = event.target as Element | null;
  if (target?.closest('[data-state="open"]')) return;
  emit('clear');
}

watch(
  () => props.count > 0,
  (visible) => {
    if (visible) window.addEventListener('keydown', onKeydown);
    else window.removeEventListener('keydown', onKeydown);
  },
  { immediate: true },
);

onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKeydown);
});
</script>

<template>
  <Transition name="bulk-bar">
    <div
      v-if="props.count > 0"
      v-bind="$attrs"
      data-slot="bulk-bar"
      class="bulk-bar"
      role="toolbar"
      aria-live="polite"
    >
      <button
        type="button"
        class="btn btn-ghost bulk-bar__clear"
        data-testid="bulk-clear"
        :aria-label="t('common.clearSelection')"
        :title="t('common.clearSelection')"
        @click="emit('clear')"
      >
        <UiIcon name="close" :size="14" />
      </button>
      <span class="bulk-bar__divider" aria-hidden="true" />
      <span class="text-fg-muted px-1 text-sm whitespace-nowrap" data-testid="bulk-count">
        {{ t('common.selectedCount', { count: props.count }) }}
      </span>
      <span class="bulk-bar__divider" aria-hidden="true" />
      <slot />
    </div>
  </Transition>
</template>

<style scoped>
.bulk-bar-enter-active,
.bulk-bar-leave-active {
  transition:
    opacity 150ms ease-out,
    transform 150ms ease-out;
}
.bulk-bar-enter-from,
.bulk-bar-leave-to {
  opacity: 0;
  /* translateX(-50%) 为水平居中所需；动画仅叠加垂直位移。 */
  transform: translate(-50%, 12px);
}
</style>
