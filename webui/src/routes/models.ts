import { createFileRoute } from '@tanstack/vue-router';
import ModelsFeature from '@/features/models/ModelsFeature.vue';
import { requireModelsPage } from '@/lib/router-guards';

export const Route = createFileRoute('/models')({
  beforeLoad: requireModelsPage(),
  component: ModelsFeature,
  staticData: { titleKey: 'nav.models' },
});
