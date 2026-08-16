import { createFileRoute } from '@tanstack/vue-router';
import ModelsFeature from '@/features/models/ModelsFeature.vue';
import { requireAuth } from '@/lib/router-guards';

export const Route = createFileRoute('/models')({
  beforeLoad: requireAuth,
  component: ModelsFeature,
  staticData: { titleKey: 'nav.models' },
});
