<script setup lang="ts">
// 表格页搜索框：前置搜索图标；有内容时尾部出现清除按钮。
import { useI18n } from 'vue-i18n';
import UiIcon from '@/components/ui/UiIcon.vue';
import { cn } from '@/lib/cn';

defineOptions({
  inheritAttrs: false,
});

const props = defineProps<{
  id: string;
  class?: string;
}>();

const { t } = useI18n();

const model = defineModel<string>({ required: true });

function clearSearch() {
  model.value = '';
}
</script>

<template>
  <div :class="cn('search-input', props.class)">
    <UiIcon name="search" :size="14" class="search-input-icon" />
    <input
      :id="props.id"
      v-model="model"
      v-bind="$attrs"
      type="text"
      class="input search-input-field"
    />
    <button
      v-if="model"
      type="button"
      class="search-input-clear"
      :aria-label="t('common.clearSearch')"
      @click="clearSearch"
    >
      <UiIcon name="close" :size="12" />
    </button>
  </div>
</template>
