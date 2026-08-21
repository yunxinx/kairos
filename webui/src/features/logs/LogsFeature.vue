<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { TabsContent, TabsIndicator, TabsList, TabsRoot, TabsTrigger } from 'reka-ui';
import { roleAtLeast } from '@/api/types';
import PageHeader from '@/app/layout/PageHeader.vue';
import RequestLogsPanel from '@/features/logs/RequestLogsPanel.vue';
import SystemLogsPanel from '@/features/logs/SystemLogsPanel.vue';
import { useCurrentUser } from '@/lib/session';

const { t } = useI18n();
const me = useCurrentUser();

/**
 * 系统日志是运维与审计视图，后端要求 admin+。
 * 普通用户不渲染该 tab，避免点进去只拿到 403。
 */
const canReadSystemLogs = computed(() => {
  const role = me.value?.role;
  return role !== undefined && roleAtLeast(role, 'admin');
});
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
          <TabsTrigger
            v-if="canReadSystemLogs"
            value="system"
            class="page-tab-switch-btn"
            data-testid="logs-tab-system"
          >
            {{ t('logs.kind.system') }}
          </TabsTrigger>
        </TabsList>
      </template>
    </PageHeader>
    <TabsContent value="request">
      <RequestLogsPanel />
    </TabsContent>
    <TabsContent v-if="canReadSystemLogs" value="system">
      <SystemLogsPanel />
    </TabsContent>
  </TabsRoot>
</template>
