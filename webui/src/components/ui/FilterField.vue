<script setup lang="ts">
// 筛选栏字段：仅 label + 控件槽，不带表单字段的 error/hint 语义。
withDefaults(
  defineProps<{
    label: string;
    inputId: string;
    /** label 显示在控件左侧；默认在控件上方。 */
    inline?: boolean;
  }>(),
  { inline: false },
);
</script>

<template>
  <label :for="inputId" :class="inline ? 'flex items-center gap-2' : 'block'">
    <span
      class="text-fg-muted text-xs font-medium"
      :class="inline ? 'shrink-0 whitespace-nowrap' : 'mb-1 block'"
    >
      {{ label }}
    </span>
    <slot />
  </label>
</template>
