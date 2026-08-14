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
    <slot />
  </th>
</template>
