import { createFileRoute } from '@tanstack/vue-router';
import ChannelFeature from '@/features/channel/ChannelFeature.vue';
import { requireRole } from '@/lib/router-guards';

export const Route = createFileRoute('/channel')({
  beforeLoad: requireRole('root'),
  component: ChannelFeature,
  staticData: { titleKey: 'nav.channel' },
});
