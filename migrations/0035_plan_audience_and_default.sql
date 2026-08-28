-- 套餐受众与「新用户默认档」：把此前硬编码在代码里的两条规则搬进数据。
--
-- `audience` 区分「给普通用户的档」与「给管理员的档」：管理面能力开关只对
-- admin 档有意义，用户档不该暴露它们。
-- `is_default` 取代 `users::default_plan_id_for_role` 里写死的 id 1 / 2，
-- 让运营能改「新用户落到哪一档」而不必改代码。
--
-- 每个受众至多一个默认档，用部分唯一索引兜住：先清零同受众再置位的写法若被
-- 并发打断，索引会挡下第二个默认档，而不是留下两个都为 1 的静默错误状态。
ALTER TABLE plans ADD COLUMN audience TEXT NOT NULL DEFAULT 'user';
ALTER TABLE plans ADD COLUMN is_default INTEGER NOT NULL DEFAULT 0;

-- 内置 admin 档（id=2）本就是管理员档；其余存量档保持 'user'。
UPDATE plans SET audience = 'admin' WHERE id = 2;

-- 存量默认档 = 迁移前代码里硬编码的那两个 id。
UPDATE plans SET is_default = 1 WHERE id IN (1, 2);

CREATE UNIQUE INDEX plan_default_per_audience
ON plans(audience) WHERE is_default = 1;
