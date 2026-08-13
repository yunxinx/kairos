<script setup lang="ts">
import { useI18n } from 'vue-i18n';

defineProps<{
  message: string;
}>();

const emit = defineEmits<{
  retry: [];
}>();

const { t } = useI18n();
</script>

<template>
  <div class="card mb-6 border-[var(--danger)] bg-[var(--danger-bg)]" role="alert">
    <div class="card-body flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
      <p class="text-danger text-sm">{{ message }}</p>
      <button type="button" class="btn btn-sm shrink-0" @click="emit('retry')">
        {{ t('common.retry') }}
      </button>
    </div>
  </div>
</template>
