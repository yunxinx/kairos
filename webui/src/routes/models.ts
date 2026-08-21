import { createFileRoute } from '@tanstack/vue-router';
import ModelsFeature from '@/features/models/ModelsFeature.vue';
import { requireRole } from '@/lib/router-guards';

export const Route = createFileRoute('/models')({
  beforeLoad: requireRole('admin'),
  component: ModelsFeature,
  staticData: { titleKey: 'nav.models' },
});
