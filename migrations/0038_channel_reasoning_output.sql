-- 渠道 reasoning 思维链兼容输出模式：auto（默认，按出站模型名 / 渠道
-- base_url 命中厂商提示词表自动开启）、always（强制开启）、off（强制关闭）。
-- 缺省 auto，存量渠道在名字不命中提示词表时行为不变。

ALTER TABLE channels ADD COLUMN reasoning_output TEXT NOT NULL DEFAULT 'auto';
