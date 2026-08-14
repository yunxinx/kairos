-- 令牌生命周期字段：启用开关、创建时间、最后使用时间（V4 令牌界面）。
-- enabled 缺省 1（存量令牌视为启用）；created_at 缺省 0（存量令牌无创建记录）；
-- last_used_at 为 NULL 表示从未使用。

ALTER TABLE tokens ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1;
ALTER TABLE tokens ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tokens ADD COLUMN last_used_at INTEGER;
