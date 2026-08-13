import { onScopeDispose, ref } from 'vue';
import {
  findFirstFieldError,
  type FieldValidationSpec,
  type ValidationTranslate,
} from '@/lib/form-validation';

const HINT_AUTO_DISMISS_MS = 1500;

/** 管理单字段渐进式提示，替代浏览器原生 constraint validation。 */
export function useFormValidation() {
  const activeError = ref<{ field: string; message: string } | null>(null);
  let dismissTimer: ReturnType<typeof setTimeout> | undefined;
  let documentListener: ((event: Event) => void) | undefined;

  function removeDocumentListener() {
    if (!documentListener) {
      return;
    }
    document.removeEventListener('pointerdown', documentListener, true);
    documentListener = undefined;
  }

  function clearDismissTimer() {
    if (dismissTimer !== undefined) {
      clearTimeout(dismissTimer);
      dismissTimer = undefined;
    }
  }

  function dismissError() {
    clearDismissTimer();
    removeDocumentListener();
    activeError.value = null;
  }

  function scheduleAutoDismiss() {
    clearDismissTimer();
    dismissTimer = setTimeout(() => {
      dismissError();
    }, HINT_AUTO_DISMISS_MS);
  }

  function attachDocumentListener() {
    removeDocumentListener();
    documentListener = (event: Event) => {
      const target = event.target;
      if (!(target instanceof Element)) {
        dismissError();
        return;
      }
      if (target.closest('.field-info-hint-trigger')) {
        return;
      }

      const activeField = activeError.value?.field;
      if (activeField) {
        const fieldRoot = target.closest(`[data-form-field="${activeField}"]`);
        if (fieldRoot) {
          const tag = target.tagName;
          const isFormControl =
            tag === 'INPUT' || tag === 'SELECT' || tag === 'TEXTAREA' || tag === 'BUTTON';
          if (!isFormControl) {
            return;
          }
        }
      }
      dismissError();
    };
    document.addEventListener('pointerdown', documentListener, true);
  }

  function showError(field: string, message: string) {
    activeError.value = { field, message };
    scheduleAutoDismiss();
    attachDocumentListener();
  }

  function fieldError(name: string): string | undefined {
    if (activeError.value?.field !== name) {
      return undefined;
    }
    return activeError.value.message;
  }

  function fieldInputHandlers(name: string) {
    return {
      onFocus: () => {
        if (activeError.value?.field === name) {
          dismissError();
        }
      },
      onPointerdown: () => {
        if (activeError.value?.field === name) {
          dismissError();
        }
      },
    };
  }

  function validate(specs: FieldValidationSpec[], t: ValidationTranslate): boolean {
    const firstError = findFirstFieldError(specs, t);
    if (firstError) {
      showError(firstError.name, firstError.message);
      return false;
    }
    dismissError();
    return true;
  }

  onScopeDispose(() => {
    dismissError();
  });

  return {
    activeError,
    fieldError,
    fieldInputHandlers,
    dismissError,
    clearErrors: dismissError,
    showFieldError: showError,
    validate,
  };
}
