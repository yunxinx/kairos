-- 管理用户与用户钱包（ADR-0008）：余额从令牌挪到用户；令牌只留归属与累计结算。
-- 存量一律归 id=1 的 root；钱包剩余 = 各令牌剩余之和；各令牌 settled 保留。
-- tokens.model_group 改为可空（删组后置空，不改绑 default；见 ADR-0010）。

CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    email TEXT NOT NULL UNIQUE COLLATE NOCASE,
    display_name TEXT NOT NULL,
    password_hash TEXT,
    role TEXT NOT NULL CHECK (role IN ('root', 'admin', 'user')),
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
) STRICT;

INSERT INTO users (id, email, display_name, password_hash, role, enabled, created_at)
VALUES (1, 'root@localhost', 'root', NULL, 'root', 1, 0);

CREATE TABLE user_balance (
    user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    balance_usd_micros INTEGER NOT NULL,
    settled_usd_micros INTEGER NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

INSERT INTO user_balance (user_id, balance_usd_micros, settled_usd_micros, created_at)
SELECT 1,
       COALESCE((SELECT SUM(balance_usd_micros) FROM token_balance), 0),
       COALESCE((SELECT SUM(settled_usd_micros) FROM token_balance), 0),
       0;

CREATE TABLE user_model_groups (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    group_name TEXT NOT NULL REFERENCES model_groups(name) ON DELETE CASCADE,
    PRIMARY KEY (user_id, group_name)
) STRICT;

INSERT INTO user_model_groups (user_id, group_name) VALUES (1, 'default');

-- 令牌重建须先复制子表 token_balance（迁移事务内不能关外键）。
CREATE TABLE tokens_new (
    token_key TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    limit_usd_micros INTEGER,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL DEFAULT 0,
    last_used_at INTEGER,
    rate_limit_rpm INTEGER,
    model_group TEXT DEFAULT 'default' REFERENCES model_groups(name) ON DELETE SET NULL,
    user_id INTEGER NOT NULL DEFAULT 1 REFERENCES users(id)
) STRICT;

INSERT INTO tokens_new (
    token_key, name, limit_usd_micros, enabled, created_at, last_used_at,
    rate_limit_rpm, model_group, user_id
)
SELECT token_key, name, limit_usd_micros, enabled, created_at, last_used_at,
       rate_limit_rpm, model_group, 1
FROM tokens;

CREATE TABLE token_balance_new (
    token_key TEXT PRIMARY KEY REFERENCES tokens_new(token_key) ON DELETE CASCADE,
    settled_usd_micros INTEGER NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

INSERT INTO token_balance_new (token_key, settled_usd_micros, created_at)
SELECT token_key, settled_usd_micros, created_at FROM token_balance;

DROP TABLE token_balance;
DROP TABLE tokens;
ALTER TABLE tokens_new RENAME TO tokens;
ALTER TABLE token_balance_new RENAME TO token_balance;

CREATE INDEX idx_tokens_user_id ON tokens(user_id);
