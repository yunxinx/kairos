<script setup lang="ts">
// 表格「模型」列：点击复制名称；悬停描边表示可点。成功/失败由 Toast 反馈。
import { useI18n } from 'vue-i18n';
import { useToast } from '@/composables/useToast';

const props = defineProps<{
  text: string;
  testId?: string;
}>();

const { t } = useI18n();
const { copy } = useToast();

function handleCopy() {
  void copy(props.text, t('common.copiedModelName'), t('common.copyFailedModelName'));
}
</script>

<template>
  <button
    type="button"
    class="copyable-name"
    :data-testid="testId"
    :title="t('common.copy')"
    @click.stop="handleCopy"
  >
    {{ text }}
  </button>
</template>
