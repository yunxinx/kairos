<script setup lang="ts">
import { cn } from '@/lib/cn';

defineOptions({ inheritAttrs: false });

const props = defineProps<{
  class?: string;
}>();
</script>

<template>
  <div data-slot="table-container" class="relative w-full overflow-x-auto">
    <table
      data-slot="table"
      v-bind="$attrs"
      :class="cn('w-full caption-bottom text-sm', props.class)"
    >
      <slot />
    </table>
  </div>
</template>
