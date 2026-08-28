<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { TabsContent, TabsIndicator, TabsList, TabsRoot, TabsTrigger } from 'reka-ui';
import PageHeader from '@/app/layout/PageHeader.vue';
import RequestLogsPanel from '@/features/logs/RequestLogsPanel.vue';
import SystemLogsPanel from '@/features/logs/SystemLogsPanel.vue';
import { hasCapability } from '@/lib/capabilities';
import { useCurrentUser } from '@/lib/session';

const { t } = useI18n();
const me = useCurrentUser();

/**
 * 该 tab 对所有登录用户开放，但两种角色看到的是不同的东西：
 * admin+ 看全量系统日志（含无操作者的运维事件），普通用户只看自己的审计行
 * （后端按身份钉死 actor 维）。标题随之区分，避免普通用户以为自己在看运维视图。
 *
 * admin 仍受 `view_logs_stats` 收窄：套餐关掉该能力时后端返回 403，不渲染入口。
 */
const isPlainUser = computed(() => me.value?.role === 'user');
const canReadSystemLogs = computed(
  () => isPlainUser.value || hasCapability(me.value, 'view_logs_stats'),
);
const systemTabLabel = computed(() =>
  isPlainUser.value ? t('logs.kind.ownAudit') : t('logs.kind.system'),
);
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
            {{ systemTabLabel }}
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
