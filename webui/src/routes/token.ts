import { createFileRoute } from '@tanstack/vue-router';
import PlaceholderFeature from '@/features/placeholders/PlaceholderFeature.vue';
import { requireAuth } from '@/lib/router-guards';

export const Route = createFileRoute('/token')({
  beforeLoad: requireAuth,
  component: PlaceholderFeature,
  staticData: { titleKey: 'nav.tokens' },
});
