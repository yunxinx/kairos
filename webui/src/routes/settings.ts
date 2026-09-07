import { createFileRoute } from '@tanstack/vue-router';
import SettingsFeature from '@/features/settings/SettingsFeature.vue';
import { requireRole } from '@/lib/router-guards';

export const Route = createFileRoute('/settings')({
  beforeLoad: requireRole('root'),
  validateSearch: (search: Record<string, unknown>): { section?: string | undefined } => ({
    section: typeof search.section === 'string' ? search.section : undefined,
  }),
  component: SettingsFeature,
  staticData: { titleKey: 'nav.settings' },
});
