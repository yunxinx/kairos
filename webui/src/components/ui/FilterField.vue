<script setup lang="ts">
// 筛选栏字段：仅 label + 控件槽，不带表单字段的 error/hint 语义。
defineProps<{
  label: string;
  inputId: string;
}>();
</script>

<template>
  <label :for="inputId" class="block">
    <span class="text-fg-muted mb-1 block text-xs font-medium">{{ label }}</span>
    <slot />
  </label>
</template>
