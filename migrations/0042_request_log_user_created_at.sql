-- 用户日志页按归属与时间范围查询；复合索引避免先扫全表再筛用户。
CREATE INDEX IF NOT EXISTS idx_request_log_user_created_at
    ON request_log (user_id, created_at);
