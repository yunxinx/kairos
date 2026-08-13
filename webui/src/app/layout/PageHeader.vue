<script setup lang="ts">
defineProps<{
  title: string;
  subtitle?: string;
}>();
</script>

<template>
  <div class="gap-stack mb-6 flex flex-col sm:mb-10 sm:flex-row sm:items-end sm:justify-between">
    <div>
      <h1 class="font-serif text-2xl font-normal">{{ title }}</h1>
      <p v-if="subtitle" class="text-fg-muted mt-1 text-sm">{{ subtitle }}</p>
    </div>
    <div class="flex flex-wrap gap-2">
      <slot name="actions" />
    </div>
  </div>
</template>
