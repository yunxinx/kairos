<script setup lang="ts">
defineOptions({
  inheritAttrs: false,
});

defineProps<{
  id: string;
  invalid?: boolean;
  /** 校验失败时关联 `FormField` 气泡的 id。 */
  hintId?: string | undefined;
}>();

const model = defineModel<string | null | undefined>();
</script>

<template>
  <textarea
    v-bind="$attrs"
    :id="id"
    v-model="model"
    class="input w-full"
    :class="{ 'input-invalid': invalid }"
    :aria-invalid="invalid ? 'true' : undefined"
    :aria-describedby="invalid && hintId ? hintId : undefined"
  />
</template>
