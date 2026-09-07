<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import UiIcon from '@/components/ui/UiIcon.vue';

defineOptions({
  inheritAttrs: false,
});

defineProps<{
  id: string;
  invalid?: boolean;
  /** 校验失败时关联 `FormField` 气泡的 id。 */
  hintId?: string | undefined;
}>();

const model = defineModel<string | number | null | undefined>();

const { t } = useI18n();
const visible = ref(false);

const inputType = computed(() => (visible.value ? 'text' : 'password'));

function toggleVisibility() {
  visible.value = !visible.value;
}
</script>

<template>
  <div class="password-input">
    <input
      v-bind="$attrs"
      :id="id"
      v-model="model"
      class="input password-input-field w-full"
      :class="{ 'input-invalid': invalid }"
      :type="inputType"
      :aria-invalid="invalid ? 'true' : undefined"
      :aria-describedby="invalid && hintId ? hintId : undefined"
    />
    <button
      type="button"
      class="password-input-toggle"
      data-testid="secret-reveal"
      :aria-label="visible ? t('a11y.hidePassword') : t('a11y.showPassword')"
      :aria-pressed="visible"
      @click="toggleVisibility"
    >
      <UiIcon class="password-input-icon" :name="visible ? 'lock-open' : 'lock'" :size="14" />
    </button>
  </div>
</template>
