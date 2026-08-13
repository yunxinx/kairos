import { createFileRoute } from '@tanstack/vue-router';
import OverviewFeature from '@/features/overview/OverviewFeature.vue';
import { requireAuth } from '@/lib/router-guards';

export const Route = createFileRoute('/overview')({
  beforeLoad: requireAuth,
  component: OverviewFeature,
  staticData: { titleKey: 'overview.title' },
});
