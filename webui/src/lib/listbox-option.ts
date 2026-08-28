/** 单选列表选择器（ListboxSelect）的选项行。 */
export interface ListboxSelectOption {
  value: string;
  label: string;
  /** 次级说明行；缺省不渲染。 */
  description?: string;
  /** 右侧小徽章（如「默认」）。 */
  badge?: string;
}
