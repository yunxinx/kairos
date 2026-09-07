import { createFileRoute } from '@tanstack/vue-router';
import ChannelFeature from '@/features/channel/ChannelFeature.vue';
import { requireRole } from '@/lib/router-guards';

export const Route = createFileRoute('/channels')({
  beforeLoad: requireRole('root'),
  validateSearch: (
    search: Record<string, unknown>,
  ): { q?: string | undefined; status?: string | undefined } => ({
    q: typeof search.q === 'string' ? search.q : undefined,
    status: typeof search.status === 'string' ? search.status : undefined,
  }),
  component: ChannelFeature,
  staticData: { titleKey: 'nav.channel' },
});
