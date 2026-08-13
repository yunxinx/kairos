import type { VirtualDataTableColumn } from '@/components/ui/virtual-data-table-columns';

/** 管理端列表常用列宽，配合 VirtualDataTable table-fixed 避免虚拟滚动列宽跳动。 */
export const managementTableColumnPresets = {
  tokens: [
    { id: 'name', width: '16%' },
    { id: 'key', width: '18%' },
    { id: 'balance', width: '9rem', minWidth: '9rem' },
    { id: 'settled', width: '9rem', minWidth: '9rem' },
    { id: 'limit', width: '8rem', minWidth: '8rem' },
    { id: 'actions', width: '1%', minWidth: '16rem' },
  ] satisfies VirtualDataTableColumn[],
  channels: [
    { id: 'name', width: '14%' },
    { id: 'protocol', width: '11rem', minWidth: '11rem' },
    { id: 'baseUrl', width: '22%' },
    { id: 'models', width: '16%' },
    { id: 'priority', width: '6rem', minWidth: '6rem' },
    { id: 'probe', width: '14rem', minWidth: '14rem' },
    { id: 'actions', width: '1%', minWidth: '14rem' },
  ] satisfies VirtualDataTableColumn[],
  pricing: [
    { id: 'model', width: '22%' },
    { id: 'input', width: '9rem', minWidth: '9rem' },
    { id: 'output', width: '9rem', minWidth: '9rem' },
    { id: 'cacheRead', width: '9rem', minWidth: '9rem' },
    { id: 'cacheWrite', width: '9rem', minWidth: '9rem' },
    { id: 'actions', width: '1%', minWidth: '10rem' },
  ] satisfies VirtualDataTableColumn[],
} as const;
