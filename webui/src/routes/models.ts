import { createFileRoute } from '@tanstack/vue-router';
import ModelsFeature from '@/features/models/ModelsFeature.vue';
import { requireModelsPage } from '@/lib/router-guards';

export const Route = createFileRoute('/models')({
  beforeLoad: requireModelsPage(),
  validateSearch: (
    search: Record<string, unknown>,
  ): {
    tab?: string | undefined;
    q?: string | undefined;
    status?: string | undefined;
    channel?: string | undefined;
  } => ({
    tab: typeof search.tab === 'string' ? search.tab : undefined,
    q: typeof search.q === 'string' ? search.q : undefined,
    status: typeof search.status === 'string' ? search.status : undefined,
    channel: typeof search.channel === 'string' ? search.channel : undefined,
  }),
  component: ModelsFeature,
  staticData: { titleKey: 'nav.models' },
});
