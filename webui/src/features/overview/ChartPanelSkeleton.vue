<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import SkeletonBlock from '@/components/ui/SkeletonBlock.vue';

const { t } = useI18n();
</script>

<template>
  <div role="status" :aria-label="t('common.loading')" class="h-full w-full">
    <SkeletonBlock height="h-chart" width="w-full" rounded="rounded" />
  </div>
</template>
