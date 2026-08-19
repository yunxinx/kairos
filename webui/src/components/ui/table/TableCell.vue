<script setup lang="ts">
import { computed, useAttrs } from 'vue';
import Tooltip from '@/components/ui/Tooltip.vue';
import { tableAlignClass, type TableAlign } from '@/components/ui/table/table-align';
import { cn } from '@/lib/cn';

defineOptions({ inheritAttrs: false });

const props = withDefaults(
  defineProps<{
    class?: string;
    align?: TableAlign;
    /** `table-layout:fixed` 下省略过长文本；需配合列宽（`max-w-0`）。完整文案走 `title` 悬浮提示。 */
    truncate?: boolean;
  }>(),
  {
    class: '',
    align: 'left',
    truncate: false,
  },
);

const attrs = useAttrs();

/** 省略列用 portal 提示；原生 `title` 在 `overflow:auto` 表格里经常出不来。 */
const overflowHint = computed(() => {
  const title = attrs.title;
  return typeof title === 'string' ? title : '';
});

const cellAttrs = computed(() => {
  if (!props.truncate) return attrs;
  const rest: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(attrs)) {
    if (key !== 'title') rest[key] = value;
  }
  return rest;
});
</script>

<template>
  <td
    data-slot="table-cell"
    v-bind="cellAttrs"
    :class="
      cn(
        'p-2 align-middle whitespace-nowrap',
        props.truncate && 'max-w-0',
        tableAlignClass[props.align],
        props.class,
      )
    "
  >
    <Tooltip v-if="props.truncate" :text="overflowHint">
      <span class="block min-w-0 truncate">
        <slot />
      </span>
    </Tooltip>
    <slot v-else />
  </td>
</template>
