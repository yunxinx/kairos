<script setup lang="ts">
import { cn } from '@/lib/cn';

const props = defineProps<{
  class?: string;
}>();
</script>

<template>
  <div data-slot="data-table-toolbar" :class="cn('flex flex-wrap items-center gap-2', props.class)">
    <slot />
  </div>
</template>
