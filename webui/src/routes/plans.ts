import { createFileRoute } from '@tanstack/vue-router';
import PlansFeature from '@/features/plans/PlansFeature.vue';
import { requireRole } from '@/lib/router-guards';

export const Route = createFileRoute('/plans')({
  beforeLoad: requireRole('root'),
  validateSearch: (search: Record<string, unknown>): { q?: string | undefined } => ({
    q: typeof search.q === 'string' ? search.q : undefined,
  }),
  component: PlansFeature,
  staticData: { titleKey: 'nav.plans' },
});
