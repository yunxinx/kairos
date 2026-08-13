<script setup lang="ts">
import type { HTMLAttributes } from 'vue';
import { cn } from '@/lib/cn';

defineOptions({ inheritAttrs: false });

const props = defineProps<{
  class?: HTMLAttributes['class'];
}>();
</script>

<template>
  <tr
    data-slot="table-row"
    v-bind="$attrs"
    :class="
      cn(
        'border-b border-[var(--seed-border)] transition-colors hover:bg-[color-mix(in_srgb,var(--seed-surface-alt)_50%,transparent)] data-[state=selected]:bg-[color-mix(in_srgb,var(--seed-surface-alt)_65%,transparent)]',
        props.class,
      )
    "
  >
    <slot />
  </tr>
</template>
