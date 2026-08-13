import { createFileRoute } from '@tanstack/vue-router';
import LogsFeature from '@/features/logs/LogsFeature.vue';
import { requireAuth } from '@/lib/router-guards';

export const Route = createFileRoute('/requests')({
  beforeLoad: requireAuth,
  component: LogsFeature,
  staticData: { titleKey: 'nav.logs', fillViewport: true },
});
