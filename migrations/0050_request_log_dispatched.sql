-- 请求日志的出站标记：1 = 已实际派发上游（含失败尝试，逐尝试行原有语义
-- 不变）；0 = 未出站即终局——准入前置失败、全部渠道冷却 / 无可用密钥、
-- 本地计费拒绝、出站安全策略拒绝等从未建立上游连接的结局。此前这类请求
-- 在 request_log 与 /stats 中完全隐形，运营无法看见被网关自身挡掉的流量。
-- 存量行一律按已派发补齐（DEFAULT 1），既有统计口径不因迁移改变；统计侧
-- 为 dispatched=0 单列计数，不并入既有出站请求口径。
ALTER TABLE request_log ADD COLUMN dispatched INTEGER NOT NULL DEFAULT 1
    CHECK (dispatched IN (0, 1));
