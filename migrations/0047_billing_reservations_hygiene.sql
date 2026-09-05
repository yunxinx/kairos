-- 预留表治理：恢复扫描候选查询（status='reserved' AND result_persisted=0
-- AND updated_at<=?）每秒执行一次，部分索引让它在新库与治理后的存量库上
-- 都是零命中快路径；终态行由后台按保留期分批清理，对账以 request_log
-- （billing_attempt_id 关联）为准。
CREATE INDEX idx_billing_reservations_recovery
    ON billing_reservations (updated_at)
    WHERE status = 'reserved' AND result_persisted = 0;
