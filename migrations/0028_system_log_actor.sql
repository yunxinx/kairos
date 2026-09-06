-- 系统日志增加操作者：审计要能查出责任人。
--
-- 用户增删改、余额调整、结算/豁免此前完全无记录——谁在什么时候给谁加了多少钱，
-- 事后查不出来。把这类事件记进 system_log（info 级，UI 的级别筛选已支持
-- info），并补一个可查询的操作者维度。
--
-- 两列都可空：既有的运维事件（结算失败、目录同步失败等）由系统自身产生，没有操作者。

ALTER TABLE system_log ADD COLUMN actor_user_id INTEGER;
-- 冗余邮箱而非只存 id：用户可被归档改名，审计行要能独立还原「当时是谁」。
ALTER TABLE system_log ADD COLUMN actor_email TEXT;

CREATE INDEX idx_system_log_actor ON system_log (actor_user_id);
