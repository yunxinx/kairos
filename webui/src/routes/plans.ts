import { createFileRoute } from '@tanstack/vue-router';
import PlansFeature from '@/features/plans/PlansFeature.vue';
import { requireRole } from '@/lib/router-guards';

export const Route = createFileRoute('/plans')({
  beforeLoad: requireRole('root'),
  component: PlansFeature,
  staticData: { titleKey: 'nav.plans' },
});
