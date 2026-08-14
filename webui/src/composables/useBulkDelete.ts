import { ref, type Ref } from 'vue';
import { useMutation, useQueryClient } from '@tanstack/vue-query';
import { extractApiError } from '@/api/client';
import type { RowSelection } from '@/composables/useRowSelection';

/** 批量删除确认窗的窗口栈载荷；各资源页并入自身 payload 联合。 */
export interface BulkDeletePayload {
  kind: 'bulk-delete';
}

interface BulkDeleteWindowEntry {
  id: number;
  payload: { kind: string };
}

interface BulkDeleteWindowStack {
  windows: Ref<BulkDeleteWindowEntry[]>;
  close(id: number): void;
}

export interface BulkDeleteOptions<K extends string | number> {
  /** 行选择状态；删除落地后同步清理选中键。 */
  selection: RowSelection<K>;
  /** 窗口栈：用于定位并关闭批量确认窗、挂载错误文案。 */
  windowStack: BulkDeleteWindowStack;
  /** 列表查询键；落地后 invalidate 重取。 */
  queryKey: readonly unknown[];
  /** 单条删除 API；管理 API 无批量端点，由本 composable 顺序逐条调用。 */
  deleteOne(key: K): Promise<unknown>;
}

export interface BulkDelete<K extends string | number> {
  /** 顺序逐条删除；任一失败即中止。 */
  mutate(keys: K[]): void;
  /** 请求进行中标忙，供确认窗禁用按钮。 */
  isPending: Ref<boolean>;
  /** 批量确认窗内展示的错误文案。 */
  error: Ref<string>;
}

/**
 * 批量删除：顺序逐条调用单删 API。
 *
 * 无论成功或部分失败都 invalidate 列表，使服务端状态成为单一事实源；
 * 部分失败时把「已成功删除」的键移出选择集，避免重试重发已删键。
 */
export function useBulkDelete<K extends string | number>(
  options: BulkDeleteOptions<K>,
): BulkDelete<K> {
  const queryClient = useQueryClient();
  const error = ref('');

  function bulkWindow(): BulkDeleteWindowEntry | undefined {
    return options.windowStack.windows.value.find((entry) => entry.payload.kind === 'bulk-delete');
  }

  const mutation = useMutation({
    mutationFn: async (keys: K[]) => {
      let done = 0;
      try {
        for (const key of keys) {
          await options.deleteOne(key);
          done += 1;
        }
      } catch (err) {
        // 以中止位置切分已删/未删：已删键随后移出选择集，未删键保留供重试。
        const succeeded = keys.slice(0, done);
        const failed = keys.slice(done);
        error.value = extractApiError(err).message;
        options.selection.setMany(failed, true);
        options.selection.setMany(succeeded, false);
        throw err;
      }
    },
    onSuccess: async () => {
      const entry = bulkWindow();
      if (entry) options.windowStack.close(entry.id);
      error.value = '';
      options.selection.clear();
      await queryClient.invalidateQueries({ queryKey: options.queryKey });
    },
    onError: async () => {
      // 部分删除已落库：重取列表让本地与服务端一致（watch 侧 prune 随之收敛）。
      await queryClient.invalidateQueries({ queryKey: options.queryKey });
    },
  });

  return {
    mutate: (keys) => mutation.mutate(keys),
    isPending: mutation.isPending,
    error,
  };
}
