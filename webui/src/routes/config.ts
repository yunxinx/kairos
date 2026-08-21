import { createFileRoute } from '@tanstack/vue-router';
import SettingsFeature from '@/features/settings/SettingsFeature.vue';
import { requireRole } from '@/lib/router-guards';

export const Route = createFileRoute('/config')({
  beforeLoad: requireRole('root'),
  component: SettingsFeature,
  staticData: { titleKey: 'nav.settings' },
});
