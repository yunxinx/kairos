import { createFileRoute } from '@tanstack/vue-router';
import TokensFeature from '@/features/tokens/TokensFeature.vue';
import { requireAuth } from '@/lib/router-guards';

export const Route = createFileRoute('/token')({
  beforeLoad: requireAuth,
  component: TokensFeature,
  staticData: { titleKey: 'nav.tokens', fillViewport: true },
});
