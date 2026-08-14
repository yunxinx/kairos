<script setup lang="ts">
import { cn } from '@/lib/cn';

defineOptions({ inheritAttrs: false });

const props = withDefaults(
  defineProps<{
    class?: string;
  }>(),
  {
    class: '',
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
        props.class,
      )
    "
  >
    <slot />
  </div>
</template>
