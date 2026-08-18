-- 令牌可选 RPM：NULL 跟随全局兜底，0 表示该令牌不限速，正数可高于全局上限。

ALTER TABLE tokens ADD COLUMN rate_limit_rpm INTEGER;
