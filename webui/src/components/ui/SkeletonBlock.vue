<script setup lang="ts">
withDefaults(
  defineProps<{
    /** Tailwind height class, e.g. h-4 */
    height?: string;
    width?: string;
    rounded?: string;
  }>(),
  {
    height: 'h-4',
    width: 'w-full',
    rounded: 'rounded-sm',
  },
);
</script>

<template>
  <div class="skeleton-block" :class="[height, width, rounded]" aria-hidden="true" />
</template>
