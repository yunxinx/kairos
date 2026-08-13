/** 表头与单元格共用的水平对齐，避免 `text-left`/`text-center` 同时出现互相覆盖。 */
export type TableAlign = 'left' | 'center' | 'right';

export const tableAlignClass: Record<TableAlign, string> = {
  left: 'text-left',
  center: 'text-center',
  right: 'text-right',
};
