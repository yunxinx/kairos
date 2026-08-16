-- 模型组：令牌绑定的可调用名允许名单（ADR-0005）。
-- 内置 `default` 不可删除；存量令牌一律绑到 default。
-- tokens.model_group 外键指向 model_groups：须先复制子表 token_balance，
-- 才能 DROP tokens（迁移事务内不能关闭 foreign_keys）。

CREATE TABLE model_groups (
    name TEXT PRIMARY KEY,
    models_json TEXT NOT NULL
) STRICT;

INSERT INTO model_groups (name, models_json) VALUES ('default', '[]');

CREATE TABLE tokens_new (
    token_key TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    limit_usd_micros INTEGER,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL DEFAULT 0,
    last_used_at INTEGER,
    model_group TEXT NOT NULL DEFAULT 'default' REFERENCES model_groups(name)
) STRICT;

INSERT INTO tokens_new (token_key, name, limit_usd_micros, enabled, created_at, last_used_at, model_group)
    SELECT token_key, name, limit_usd_micros, enabled, created_at, last_used_at, 'default' FROM tokens;

CREATE TABLE token_balance_new (
    token_key TEXT PRIMARY KEY REFERENCES tokens_new(token_key) ON DELETE CASCADE,
    balance_usd_micros INTEGER NOT NULL,
    settled_usd_micros INTEGER NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

INSERT INTO token_balance_new (token_key, balance_usd_micros, settled_usd_micros, created_at)
    SELECT token_key, balance_usd_micros, settled_usd_micros, created_at FROM token_balance;

DROP TABLE token_balance;
DROP TABLE tokens;
ALTER TABLE tokens_new RENAME TO tokens;
ALTER TABLE token_balance_new RENAME TO token_balance;
