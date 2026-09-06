-- 渠道自动缓存断点注入开关：0（默认）不为出站请求补缓存断点，1 按序
-- （tools 尾 → system 尾 → 末条消息尾块）自动注入。缺省关，存量渠道
-- 出站行为不变。

ALTER TABLE channels ADD COLUMN injects_cache_breakpoints INTEGER NOT NULL DEFAULT 0;
