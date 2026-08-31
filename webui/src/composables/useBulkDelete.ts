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
  close(id: number, force?: boolean): boolean;
}

export interface BulkDeleteOptions<K extends string | number> {
  /** 行选择状态；删除落地后同步清理选中键。 */
  selection: RowSelection<K>;
  /** 窗口栈：用于定位并关闭批量确认窗、挂载错误文案。 */
  windowStack: BulkDeleteWindowStack;
  /** 列表查询键；落地后 invalidate 重取。 */
  queryKey: readonly unknown[];
  /**
   * 同一份数据的其他投影键；与 `queryKey` 一并失效。
   *
   * 渠道有「完整定义」与「名录」两份缓存，只重取列表那一份会让模型页继续按
   * 已删渠道渲染。
   */
  alsoInvalidate?: readonly (readonly unknown[])[];
  /** 事务式集合删除 API；服务端保证整批成功或整批回滚。 */
  deleteMany(keys: K[]): Promise<unknown>;
}

export interface BulkDelete<K extends string | number> {
  /** 一次提交整批目标。 */
  mutate(keys: K[]): void;
  /** 请求进行中标忙，供确认窗禁用按钮。 */
  isPending: Ref<boolean>;
  /** 批量确认窗内展示的错误文案。 */
  error: Ref<string>;
}

/**
 * 批量删除：一次调用事务式集合端点，失败时服务端状态不变，选择集保留供修正后重试。
 */
export function useBulkDelete<K extends string | number>(
  options: BulkDeleteOptions<K>,
): BulkDelete<K> {
  const queryClient = useQueryClient();
  const error = ref('');

  function bulkWindow(): BulkDeleteWindowEntry | undefined {
    return options.windowStack.windows.value.find((entry) => entry.payload.kind === 'bulk-delete');
  }

  /** 列表键与其余投影键一起失效。 */
  async function invalidateAll(): Promise<void> {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: options.queryKey }),
      ...(options.alsoInvalidate ?? []).map((key) =>
        queryClient.invalidateQueries({ queryKey: key }),
      ),
    ]);
  }

  const mutation = useMutation({
    mutationFn: (keys: K[]) => options.deleteMany(keys),
    onSuccess: async () => {
      const entry = bulkWindow();
      if (entry) options.windowStack.close(entry.id, true);
      error.value = '';
      options.selection.clear();
      await invalidateAll();
    },
    onError: (err) => {
      error.value = extractApiError(err).message;
    },
  });

  return {
    mutate: (keys) => mutation.mutate(keys),
    isPending: mutation.isPending,
    error,
  };
}
