<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import SkeletonBlock from '@/components/ui/SkeletonBlock.vue';

const { t } = useI18n();
</script>

<template>
  <div role="status" :aria-label="t('common.loading')" class="h-full min-h-0 w-full">
    <SkeletonBlock height="h-full" width="w-full" rounded="rounded" />
  </div>
</template>
