<script setup lang="ts">
// 表格页搜索框：前置搜索图标；有内容时尾部出现清除按钮。
// 清除用 mousedown.prevent：不抢焦点，避免失焦态下点击叉号触发布局变化导致 click 丢失。
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
      data-testid="search-input-clear"
      @mousedown.prevent="clearSearch"
    >
      <UiIcon name="close" :size="12" />
    </button>
  </div>
</template>
