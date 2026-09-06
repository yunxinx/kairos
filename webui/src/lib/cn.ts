import type { HTMLAttributes } from 'vue';

type ClassNameInput = HTMLAttributes['class'];

/** 合并 Tailwind 类名，并忽略空值与重复冲突。 */
export function cn(...parts: ClassNameInput[]): string {
  return parts
    .flatMap((part) => {
      if (!part) {
        return [];
      }
      if (typeof part === 'string') {
        return [part];
      }
      if (Array.isArray(part)) {
        const strings: string[] = [];
        for (const item of part) {
          if (typeof item === 'string') {
            strings.push(item);
          }
        }
        return strings;
      }
      if (typeof part === 'object') {
        return Object.entries(part)
          .filter(([, enabled]) => enabled)
          .map(([key]) => key);
      }
      return [];
    })
    .join(' ');
}
