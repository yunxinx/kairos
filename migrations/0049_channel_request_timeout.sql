-- 渠道级预首字节总时限：约束该渠道的连接、响应头、流首 peek 与同渠道
-- 重试退避（共享本预算）；failover 换渠道 / 统一模型换成员按新渠道重锚。
-- 缺省 120000ms 与原全局硬编码一致，存量渠道行为不变。流建立后的读取
-- 与下发仍只受渠道空闲超时（timeout_ms）约束，与本字段无关。
ALTER TABLE channels ADD COLUMN request_timeout_ms INTEGER NOT NULL DEFAULT 120000
    CHECK (request_timeout_ms > 0);
