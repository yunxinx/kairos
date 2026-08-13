import { createFileRoute } from '@tanstack/vue-router';
import ChannelFeature from '@/features/channel/ChannelFeature.vue';
import { requireAuth } from '@/lib/router-guards';

export const Route = createFileRoute('/channel')({
  beforeLoad: requireAuth,
  component: ChannelFeature,
  staticData: { titleKey: 'nav.channel', fillViewport: true },
});
