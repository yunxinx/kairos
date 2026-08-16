-- 统一模型：下游可调用名 + 有序已登记模型列表 + 隐藏开关（ADR-0004 / ADR-0006）。
-- 统一 ID 本身无价格行；计费按实际打到的成员。hide 以 0/1 整数落库。

CREATE TABLE unified_models (
    id TEXT PRIMARY KEY,
    models_json TEXT NOT NULL,
    hide INTEGER NOT NULL DEFAULT 0
) STRICT;
