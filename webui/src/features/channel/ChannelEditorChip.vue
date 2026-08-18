<script setup lang="ts">
// 编辑器模型清单的一枚 chip：点击复制、叉号删除；别名互指走 tooltip，底色由 hasAliasRelation 显式决定。
import { useI18n } from 'vue-i18n';
import Tooltip from '@/components/ui/Tooltip.vue';
import UiIcon from '@/components/ui/UiIcon.vue';

const props = defineProps<{
  name: string;
  tooltip: string;
  hasAliasRelation: boolean;
}>();

const emit = defineEmits<{
  copy: [];
  remove: [];
}>();

const { t } = useI18n();

/** 仅 chip 本体聚焦时复制，内部删除按钮的按键不拦截。 */
function onChipKeydown(event: KeyboardEvent) {
  if (event.target !== event.currentTarget) return;
  event.preventDefault();
  emit('copy');
}
</script>

<template>
  <Tooltip :text="props.tooltip">
    <div
      role="button"
      tabindex="0"
      class="flex cursor-pointer items-center gap-1 rounded-md py-1 pr-1 pl-2"
      :class="props.hasAliasRelation ? 'model-chip-alias' : 'bg-[var(--seed-surface-alt)]'"
      data-testid="channel-model-chip"
      :data-model="props.name"
      @click="emit('copy')"
      @keydown.enter="onChipKeydown"
      @keydown.space="onChipKeydown"
    >
      <span class="min-w-0 flex-1 truncate font-mono text-xs">{{ props.name }}</span>
      <button
        type="button"
        class="text-fg-subtle hover:text-danger cursor-pointer rounded p-0.5 hover:bg-[var(--danger-bg)]"
        data-testid="channel-model-remove"
        :aria-label="t('channel.removeModel', { model: props.name })"
        @click.stop="emit('remove')"
      >
        <UiIcon name="close" :size="12" />
      </button>
    </div>
  </Tooltip>
</template>
