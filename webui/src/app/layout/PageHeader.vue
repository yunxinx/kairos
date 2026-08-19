<script setup lang="ts">
defineProps<{
  title?: string;
}>();
</script>

<template>
  <div class="gap-stack mb-6 flex flex-col sm:mb-10 sm:flex-row sm:items-center sm:justify-between">
    <div class="page-header-leading">
      <slot name="leading">
        <h1 v-if="title" class="page-header-title font-normal">{{ title }}</h1>
      </slot>
    </div>
    <div v-if="$slots.actions" class="flex flex-wrap gap-2">
      <slot name="actions" />
    </div>
  </div>
</template>
