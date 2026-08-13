<script setup lang="ts">
defineProps<{
  title: string;
}>();
</script>

<template>
  <div class="gap-stack mb-6 flex flex-col sm:mb-10 sm:flex-row sm:items-center sm:justify-between">
    <div>
      <h1 class="font-serif text-2xl font-normal">{{ title }}</h1>
    </div>
    <div v-if="$slots.actions" class="flex flex-wrap gap-2">
      <slot name="actions" />
    </div>
  </div>
</template>
