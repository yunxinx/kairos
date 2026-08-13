<script setup lang="ts">
import { computed } from 'vue';
import { useRouterState } from '@tanstack/vue-router';
import { useI18n } from 'vue-i18n';
import PageHeader from '@/app/layout/PageHeader.vue';

const { t } = useI18n();
const matches = useRouterState({ select: (state) => state.matches });

const titleKey = computed(() => {
  for (let index = matches.value.length - 1; index >= 0; index -= 1) {
    const key = matches.value[index]?.staticData.titleKey;
    if (key) {
      return key;
    }
  }
  return 'app.title';
});
</script>

<template>
  <div>
    <PageHeader :title="t(titleKey)" :subtitle="t('placeholder.body')" />
    <div class="card">
      <div class="card-body">
        <p class="text-fg-muted text-sm">{{ t('placeholder.comingTitle') }}</p>
      </div>
    </div>
  </div>
</template>
