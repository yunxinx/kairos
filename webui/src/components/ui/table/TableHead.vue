<script setup lang="ts">
import { cn } from '@/lib/cn';

defineOptions({ inheritAttrs: false });

const props = defineProps<{
  class?: string;
}>();
</script>

<template>
  <th
    data-slot="table-head"
    v-bind="$attrs"
    :class="
      cn(
        'h-10 px-2 text-left align-middle text-sm font-medium whitespace-nowrap text-[var(--seed-fg)] [&:has([role=checkbox])]:pr-0 [&>[role=checkbox]]:translate-y-[2px]',
        props.class,
      )
    "
  >
    <slot />
  </th>
</template>
