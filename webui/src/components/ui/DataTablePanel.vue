<script setup lang="ts">
import { cn } from '@/lib/cn';

defineOptions({ inheritAttrs: false });

const props = withDefaults(
  defineProps<{
    class?: string;
    /** 在 flex 列布局中占满剩余高度，供表体在边框内滚动。 */
    fillViewport?: boolean;
  }>(),
  {
    class: '',
    fillViewport: false,
  },
);
</script>

<template>
  <div
    data-slot="data-table-panel"
    v-bind="$attrs"
    :class="
      cn(
        'overflow-hidden rounded-md border border-[var(--seed-border)] bg-[var(--seed-surface)]',
        // 只用 flex-1 + min-h-0：h-full 会在 flex 首帧按父级 100% 撑高，再被兄弟项挤回，造成展开回弹
        props.fillViewport && 'flex min-h-0 flex-1 flex-col',
        props.class,
      )
    "
  >
    <slot />
  </div>
</template>
