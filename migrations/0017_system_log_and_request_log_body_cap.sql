-- 系统日志与请求日志分开：请求日志仍是计费对账行；系统日志承载结算失败、
-- 落库失败、目录同步失败等运维事件，供管理面单独查询。
CREATE TABLE system_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at INTEGER NOT NULL,
    level TEXT NOT NULL,
    target TEXT NOT NULL,
    message TEXT NOT NULL
) STRICT;

CREATE INDEX idx_system_log_created_at ON system_log (created_at);
