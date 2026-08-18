<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import { TabsContent, TabsIndicator, TabsList, TabsRoot, TabsTrigger } from 'reka-ui';
import PageHeader from '@/app/layout/PageHeader.vue';
import RequestLogsPanel from '@/features/logs/RequestLogsPanel.vue';
import SystemLogsPanel from '@/features/logs/SystemLogsPanel.vue';

const { t } = useI18n();
</script>

<template>
  <TabsRoot default-value="request" class="flex flex-col">
    <PageHeader>
      <template #leading>
        <TabsList class="page-tab-switch" :aria-label="t('logs.kinds')">
          <TabsIndicator class="page-tab-switch-knob" />
          <TabsTrigger value="request" class="page-tab-switch-btn" data-testid="logs-tab-request">
            {{ t('logs.kind.request') }}
          </TabsTrigger>
          <TabsTrigger value="system" class="page-tab-switch-btn" data-testid="logs-tab-system">
            {{ t('logs.kind.system') }}
          </TabsTrigger>
        </TabsList>
      </template>
    </PageHeader>
    <TabsContent value="request">
      <RequestLogsPanel />
    </TabsContent>
    <TabsContent value="system">
      <SystemLogsPanel />
    </TabsContent>
  </TabsRoot>
</template>
