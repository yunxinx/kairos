/** VirtualDataTable 列宽定义：table-layout: fixed 下避免虚拟滚动时列宽跳动。 */
export interface VirtualDataTableColumn {
  id: string;
  /** 列宽，如 `12rem`、`18%`。 */
  width: string;
  /** 表格最小宽度累加用，防止窄屏挤压可读性。 */
  minWidth?: string;
}

/** 根据列定义生成 `<colgroup>` 用的 style。 */
export function virtualDataTableMinWidth(columns: VirtualDataTableColumn[]): string | undefined {
  const parts = columns
    .map((column) => column.minWidth ?? column.width)
    .filter((value) => value.endsWith('rem') || value.endsWith('px'));
  if (parts.length === 0) {
    return undefined;
  }
  const totalRem = parts
    .filter((value) => value.endsWith('rem'))
    .reduce((sum, value) => sum + Number.parseFloat(value), 0);
  if (totalRem > 0) {
    return `${totalRem}rem`;
  }
  return undefined;
}
