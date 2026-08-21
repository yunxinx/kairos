-- 用户软删除：停用并归档，保留历史消费记录。
--
-- 硬删除会连带毁掉对账链——`request_log` 按 `user_id` 归属，删掉用户行后这些消费
-- 记录就成了孤儿；而 `tokens.user_id` 是 NOT NULL 无级联的外键，硬删还会直接撞
-- FOREIGN KEY 约束返 500。参考 one-api（model/user.go 的 Delete 改状态 + 改名 + 拉黑）
-- 改为软删除：`deleted_at` 非 NULL 即视为已归档，所有读路径过滤掉。
--
-- 邮箱同时改写为 `deleted.{id}.{原邮箱}`：`users.email` 是列级 UNIQUE COLLATE NOCASE，
-- SQLite 无法单独去掉列级 UNIQUE 改成 partial index，而重建 users 要连带重建 4 张
-- 子表（user_balance / user_model_groups / management_sessions / tokens）。改写邮箱
-- 释放原地址供重新注册，且原地址仍内嵌可读，便于审计。

ALTER TABLE users ADD COLUMN deleted_at INTEGER;

-- 归档用户的查询都带 `deleted_at IS NULL`，按该列建部分索引即可覆盖。
CREATE INDEX idx_users_active ON users(deleted_at) WHERE deleted_at IS NULL;
