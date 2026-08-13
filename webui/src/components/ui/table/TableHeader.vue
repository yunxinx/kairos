<script setup lang="ts">
import { cn } from '@/lib/cn';

defineOptions({ inheritAttrs: false });

const props = defineProps<{
  class?: string;
}>();
</script>

<template>
  <thead data-slot="table-header" v-bind="$attrs" :class="cn('[&_tr]:border-b', props.class)">
    <slot />
  </thead>
</template>
