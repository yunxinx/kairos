-- 记录上游是否明确提供 usage，区分显式零用量与缺失用量的保守结算。
ALTER TABLE request_log ADD COLUMN usage_reported INTEGER NOT NULL DEFAULT 0
    CHECK (usage_reported IN (0, 1));
