<script setup lang="ts">
defineOptions({
  inheritAttrs: false,
});

defineProps<{
  id: string;
}>();

const model = defineModel<boolean>({ required: true });
</script>

<template>
  <label class="form-switch" :for="id">
    <input
      v-bind="$attrs"
      :id="id"
      v-model="model"
      type="checkbox"
      role="switch"
      class="form-switch-input"
      :aria-checked="model"
    />
    <span class="toggle-track" :class="{ active: model }" aria-hidden="true" />
  </label>
</template>
