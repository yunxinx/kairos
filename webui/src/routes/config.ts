import { createFileRoute } from '@tanstack/vue-router';
import SettingsFeature from '@/features/settings/SettingsFeature.vue';
import { requireAuth } from '@/lib/router-guards';

export const Route = createFileRoute('/config')({
  beforeLoad: requireAuth,
  component: SettingsFeature,
  staticData: { titleKey: 'nav.settings' },
});
