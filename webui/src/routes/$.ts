import { createFileRoute } from '@tanstack/vue-router';
import NotFoundFeature from '@/features/errors/NotFoundFeature.vue';

export const Route = createFileRoute('/$')({
  component: NotFoundFeature,
  staticData: { titleKey: 'errors.notFoundTitle' },
});
