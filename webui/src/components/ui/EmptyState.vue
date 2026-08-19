<script setup lang="ts">
defineProps<{
  title: string;
  description?: string;
}>();
</script>

<template>
  <div class="px-4 py-6 text-center">
    <p class="text-fg text-base font-semibold">{{ title }}</p>
    <p v-if="description" class="text-fg-muted mx-auto mt-2 max-w-md text-sm">
      {{ description }}
    </p>
    <div v-if="$slots.default" class="mt-4 flex justify-center gap-2">
      <slot />
    </div>
  </div>
</template>
