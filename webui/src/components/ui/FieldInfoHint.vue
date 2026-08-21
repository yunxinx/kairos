<script setup lang="ts">
import { computed, ref, useId } from 'vue';
import { useI18n } from 'vue-i18n';
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from 'reka-ui';
import UiIcon from '@/components/ui/UiIcon.vue';

const props = defineProps<{
  contentId?: string | undefined;
}>();

const { t } = useI18n();
const fallbackContentId = useId();
const resolvedContentId = computed(() => props.contentId ?? fallbackContentId);

const open = ref(false);
</script>

<template>
  <PopoverRoot v-model:open="open">
    <PopoverTrigger as-child>
      <button
        type="button"
        class="field-info-hint-trigger"
        :aria-label="t('a11y.fieldGuide')"
        aria-haspopup="dialog"
        :aria-expanded="open ? 'true' : 'false'"
        :aria-describedby="open ? resolvedContentId : undefined"
        @mousedown.prevent
      >
        <UiIcon name="circle-alert" :size="11" />
      </button>
    </PopoverTrigger>
    <PopoverPortal>
      <!-- 钉在标签行上方：说明是点开的气泡，不能落到输入框下面把表单撑开。 -->
      <PopoverContent
        :id="resolvedContentId"
        class="field-info-hint-content"
        :side-offset="6"
        side="top"
        align="start"
      >
        <div class="field-info-hint-body">
          <slot />
        </div>
      </PopoverContent>
    </PopoverPortal>
  </PopoverRoot>
</template>
