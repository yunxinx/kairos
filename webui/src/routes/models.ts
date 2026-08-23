import { createFileRoute } from '@tanstack/vue-router';
import ModelsFeature from '@/features/models/ModelsFeature.vue';
import { requireAnyCapability } from '@/lib/router-guards';

export const Route = createFileRoute('/models')({
  beforeLoad: requireAnyCapability(['edit_prices', 'edit_model_groups', 'edit_unified_models', 'edit_price_catalog', 'view_own_plan_groups', 'view_other_groups']),
  component: ModelsFeature,
  staticData: { titleKey: 'nav.models' },
});
