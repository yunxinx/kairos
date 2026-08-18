<script setup lang="ts">
import { cn } from '@/lib/cn';
import { tableAlignClass, type TableAlign } from '@/components/ui/table/table-align';

defineOptions({ inheritAttrs: false });

const props = withDefaults(
  defineProps<{
    class?: string;
    align?: TableAlign;
    /** `table-layout:fixed` 下省略过长文本；需配合列宽（`max-w-0 truncate`）。 */
    truncate?: boolean;
  }>(),
  {
    class: '',
    align: 'left',
    truncate: false,
  },
);
</script>

<template>
  <td
    data-slot="table-cell"
    v-bind="$attrs"
    :class="
      cn(
        'p-2 align-middle whitespace-nowrap',
        props.truncate && 'max-w-0 truncate',
        tableAlignClass[props.align],
        props.class,
      )
    "
  >
    <slot />
  </td>
</template>
