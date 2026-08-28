-- 用户钱包与令牌余额命令的幂等结果。
--
-- operation_id 由客户端为一次用户意图生成，并按操作者隔离作用域。业务写、审计行
-- 与本记录在同一 BEGIN IMMEDIATE 事务中提交；网络重试命中同一记录时直接返回原始前后值。
CREATE TABLE balance_operations (
    operation_id TEXT NOT NULL
        CHECK (length(operation_id) BETWEEN 1 AND 128),
    target_kind TEXT NOT NULL
        CHECK (target_kind IN ('user_wallet', 'token_balance')),
    target_id INTEGER NOT NULL,
    actor_user_id INTEGER NOT NULL,
    operation_kind TEXT NOT NULL
        CHECK (operation_kind IN ('adjust', 'set_finite', 'set_unlimited')),
    amount_usd_micros INTEGER,
    reason TEXT,
    before_usd_micros INTEGER,
    after_usd_micros INTEGER,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (target_kind, target_id, actor_user_id, operation_id),
    CHECK (
        (operation_kind = 'adjust' AND amount_usd_micros IS NOT NULL) OR
        (operation_kind = 'set_finite' AND target_kind = 'token_balance'
            AND amount_usd_micros IS NOT NULL) OR
        (operation_kind = 'set_unlimited' AND target_kind = 'token_balance'
            AND amount_usd_micros IS NULL)
    )
) STRICT;

CREATE INDEX balance_operations_target
ON balance_operations(target_kind, target_id, created_at DESC);
