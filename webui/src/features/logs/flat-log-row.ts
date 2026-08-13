/** 日志表虚拟行：主行 + 可选展开的 body 详情行。 */
export type FlatLogRow = { kind: 'main' | 'detail'; itemIndex: number };
