-- 渠道启用开关：禁用的渠道不参与路由候选与失败切换（与令牌 enabled 语义对齐）。
-- enabled 缺省 1（存量渠道视为启用）。

ALTER TABLE channels ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1;
