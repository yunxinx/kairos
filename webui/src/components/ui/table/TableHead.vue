<script setup lang="ts">
// 表头单元格。`resizable` 挂右缘拖拽把手：拖动回调给页面（页面改列宽），
// 基准宽度由把手在交互时从本 th 实测，页面无须传宽值。
import { cn } from '@/lib/cn';
import ColumnResizeHandle from '@/components/ui/table/ColumnResizeHandle.vue';
import { tableAlignClass, type TableAlign } from '@/components/ui/table/table-align';

defineOptions({ inheritAttrs: false });

const props = withDefaults(
  defineProps<{
    class?: string;
    align?: TableAlign;
    /** 列拖拽调宽的把手；`id` 与列宽持久化的键对齐。 */
    resizable?: { id: string } | undefined;
  }>(),
  {
    class: '',
    align: 'left',
    resizable: undefined,
  },
);

const emit = defineEmits<{
  resize: [id: string, width: number];
  /** 一次拖拽/键盘调整结束：落盘时机。 */
  resizeCommit: [];
  resizeReset: [id: string];
}>();
</script>

<template>
  <th
    data-slot="table-head"
    v-bind="$attrs"
    :class="
      cn(
        'h-10 bg-[var(--seed-surface-alt)] px-2 align-middle text-sm font-medium whitespace-nowrap text-[var(--seed-fg)]',
        tableAlignClass[props.align],
        props.class,
      )
    "
  >
    <div class="flex min-w-0 items-center">
      <div class="min-w-0 flex-1">
        <slot />
      </div>
      <ColumnResizeHandle
        v-if="resizable !== undefined"
        :column-id="resizable.id"
        @resize="(id, width) => emit('resize', id, width)"
        @commit="emit('resizeCommit')"
        @reset="(id) => emit('resizeReset', id)"
      />
    </div>
  </th>
</template>
