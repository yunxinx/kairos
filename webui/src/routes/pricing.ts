import { createFileRoute } from '@tanstack/vue-router';
import PricingFeature from '@/features/pricing/PricingFeature.vue';
import { requireAuth } from '@/lib/router-guards';

export const Route = createFileRoute('/pricing')({
  beforeLoad: requireAuth,
  component: PricingFeature,
  staticData: { titleKey: 'nav.pricing' },
});
