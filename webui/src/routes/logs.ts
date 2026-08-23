import { createFileRoute, redirect } from '@tanstack/vue-router';
import LogsFeature from '@/features/logs/LogsFeature.vue';
import { requireAuth } from '@/lib/router-guards';
import { hasCapability } from '@/lib/capabilities';
import { getMe } from '@/lib/session';

async function beforeLoad() {
  await requireAuth();
  const me = getMe();
  if (me && me.role === 'admin' && !hasCapability(me, 'view_logs_stats')) {
    throw redirect({ to: '/overview' });
  }
}

export const Route = createFileRoute('/logs')({
  beforeLoad,
  component: LogsFeature,
  staticData: { titleKey: 'nav.logs' },
});
