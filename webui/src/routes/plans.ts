import { createFileRoute } from '@tanstack/vue-router';
import PlansFeature from '@/features/plans/PlansFeature.vue';
import { requireRole } from '@/lib/router-guards';

export const Route = createFileRoute('/plans')({
  beforeLoad: requireRole('root'),
  validateSearch: (
    search: Record<string, unknown>,
  ): {
    q?: string | undefined;
    audience?: string | undefined;
    flag?: string | undefined;
  } => ({
    q: typeof search.q === 'string' ? search.q : undefined,
    audience: typeof search.audience === 'string' ? search.audience : undefined,
    flag: typeof search.flag === 'string' ? search.flag : undefined,
  }),
  component: PlansFeature,
  staticData: { titleKey: 'nav.plans' },
});
