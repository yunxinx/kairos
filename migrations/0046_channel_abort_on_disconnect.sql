-- 渠道断连止损开关：下游断开（响应通道关闭）后是否立即取消上游流消费。
-- 1（缺省）：立即停止读取上游字节流，按已嗅探 usage 结算，无 usage 时释放
-- 预留并告警；0：维持原语义，继续消费上游至自然收尾、按实际 usage 结算。
-- 存量渠道由 DEFAULT 1 补齐，行为与新缺省一致。

ALTER TABLE channels ADD COLUMN abort_on_disconnect INTEGER NOT NULL DEFAULT 1
    CHECK (abort_on_disconnect IN (0, 1));
