/**
 * 渠道「设置模型」同步的密钥来源，编辑器与同步视图共用：一切以当前草稿态
 * 为主——表单填了新密钥就用新密钥（`typed`）；编辑既有渠道且未改密钥
 * （留空 = 保留原值）时按库中定义取（`saved`）。
 */
export type SyncKeySource =
  | { kind: 'typed'; apiKey: string }
  | { kind: 'saved'; channelId: number };
