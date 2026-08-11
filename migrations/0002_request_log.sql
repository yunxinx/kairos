-- 请求日志：每次请求落一条元数据（时间、令牌、入站协议、模型、渠道、状态码、延迟）。
-- usage 四分量与费用列在计费票据（#04）接入。
CREATE TABLE IF NOT EXISTS request_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at INTEGER NOT NULL,          -- unix 毫秒
    token_name TEXT NOT NULL,
    inbound_protocol TEXT NOT NULL,
    model TEXT NOT NULL,
    channel TEXT NOT NULL,
    status_code INTEGER NOT NULL,
    latency_ms INTEGER NOT NULL
);
