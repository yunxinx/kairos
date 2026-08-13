<script setup lang="ts">
import { cn } from '@/lib/cn';

defineOptions({ inheritAttrs: false });

const props = defineProps<{
  class?: string;
}>();
</script>

<template>
  <td
    data-slot="table-cell"
    v-bind="$attrs"
    :class="
      cn(
        'p-2 align-middle whitespace-nowrap [&:has([role=checkbox])]:pr-0 [&>[role=checkbox]]:translate-y-[2px]',
        props.class,
      )
    "
  >
    <slot />
  </td>
</template>
