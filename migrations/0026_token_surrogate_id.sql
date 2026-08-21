-- 令牌的稳定身份与密钥分离：新增代理主键 `id`，`token_key` 退为唯一密钥列。
--
-- 起因：`token_key` 既是主键又是密钥，管理面「按 id 定位」与「不泄露明文」不能兼得——
-- admin 要禁用普通用户的令牌就必须先拿到明文 key。渠道早已是「库生成 id + 展示名」
-- 的形状（ChannelRecord），令牌是唯一的例外；对齐它即可让 admin 按 id 操作、
-- 明文 key 只对所有者返回。
--
-- `token_balance` 的外键继续指向 `tokens(token_key)`：该列仍是 NOT NULL UNIQUE，
-- 满足外键父键要求。不改成指向 `id`，是因为 key 不可轮换、改了只会把结算热路径
-- （settle_charge / ensure_token_balance）拖进一次无收益的重写。
--
-- 重建顺序照 0021：先把子表 token_balance 复制出来，再删子表、删父表、改名回填。
-- 迁移事务内不能关外键，必须先断开引用。

CREATE TABLE token_balance_carry (
    token_key TEXT NOT NULL,
    settled_usd_micros INTEGER NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;
INSERT INTO token_balance_carry (token_key, settled_usd_micros, created_at)
SELECT token_key, settled_usd_micros, created_at FROM token_balance;
DROP TABLE token_balance;

CREATE TABLE tokens_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    token_key TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    limit_usd_micros INTEGER,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL DEFAULT 0,
    last_used_at INTEGER,
    rate_limit_rpm INTEGER,
    model_group TEXT DEFAULT 'default' REFERENCES model_groups(name) ON DELETE SET NULL,
    user_id INTEGER NOT NULL DEFAULT 1 REFERENCES users(id)
) STRICT;

-- 按创建时刻发号，使存量令牌的 id 顺序与列表默认顺序一致。
INSERT INTO tokens_new (
    token_key, name, limit_usd_micros, enabled, created_at, last_used_at,
    rate_limit_rpm, model_group, user_id
)
SELECT token_key, name, limit_usd_micros, enabled, created_at, last_used_at,
       rate_limit_rpm, model_group, user_id
FROM tokens
ORDER BY created_at, token_key;

DROP TABLE tokens;
ALTER TABLE tokens_new RENAME TO tokens;

CREATE TABLE token_balance (
    token_key TEXT PRIMARY KEY REFERENCES tokens(token_key) ON DELETE CASCADE,
    settled_usd_micros INTEGER NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;
INSERT INTO token_balance (token_key, settled_usd_micros, created_at)
SELECT token_key, settled_usd_micros, created_at FROM token_balance_carry;
DROP TABLE token_balance_carry;

CREATE INDEX idx_tokens_user_id ON tokens(user_id);
