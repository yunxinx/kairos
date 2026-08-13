<script setup lang="ts">
import { cn } from '@/lib/cn';
import { tableAlignClass, type TableAlign } from '@/components/ui/table/table-align';

defineOptions({ inheritAttrs: false });

const props = withDefaults(
  defineProps<{
    class?: string;
    align?: TableAlign;
  }>(),
  {
    class: '',
    align: 'left',
  },
);
</script>

<template>
  <td
    data-slot="table-cell"
    v-bind="$attrs"
    :class="
      cn(
        'p-2 align-middle whitespace-nowrap [&:has([role=checkbox])]:pr-0 [&>[role=checkbox]]:translate-y-[2px]',
        tableAlignClass[props.align],
        props.class,
      )
    "
  >
    <slot />
  </td>
</template>
