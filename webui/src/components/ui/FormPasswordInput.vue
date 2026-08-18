<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import UiIcon from '@/components/ui/UiIcon.vue';
import { maskTokenKey } from '@/lib/format';

defineOptions({
  inheritAttrs: false,
});

const props = defineProps<{
  id: string;
  invalid?: boolean;
  /** 校验失败时关联 `FormField` 气泡的 id。 */
  hintId?: string | undefined;
  /** 锁定时按 `maskTokenKey` 展示（短密钥仍用 password 圆点，避免 ≤16 字符原样露出）。 */
  maskWhileHidden?: boolean;
}>();

const model = defineModel<string | number | null | undefined>();

const { t } = useI18n();
const visible = ref(false);

/** 与 `maskTokenKey` 一致：不超过此前缀+后缀长度时掩码等于原文。 */
const TOKEN_KEY_MASK_MIN = 16;

const keyText = computed(() => String(model.value ?? ''));
const showMaskedText = computed(
  () => props.maskWhileHidden && !visible.value && keyText.value.length > TOKEN_KEY_MASK_MIN,
);
const maskedDisplay = computed(() => maskTokenKey(keyText.value));
const inputType = computed(() => (visible.value ? 'text' : 'password'));

function toggleVisibility() {
  visible.value = !visible.value;
}
</script>

<template>
  <div class="password-input">
    <input
      v-if="showMaskedText"
      v-bind="$attrs"
      :id="id"
      class="input password-input-field w-full"
      :class="{ 'input-invalid': invalid }"
      type="text"
      readonly
      :value="maskedDisplay"
      :aria-invalid="invalid ? 'true' : undefined"
      :aria-describedby="invalid && hintId ? hintId : undefined"
      data-testid="secret-input-masked"
    />
    <input
      v-else
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
