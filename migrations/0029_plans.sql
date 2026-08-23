-- 套餐：命名的运营预设，替代人头上的 user_model_groups（ADR-0010 修订）。
--
-- 每个普通用户/管理员恰好挂一档；root（id=1）不挂档，plan_id 为 NULL。
-- 内置两档固定 id：1 = standard、2 = admin，builtin=1 不可删除；
-- 内部名与显示名可改，id 不变。
-- 套餐模型组名单是可用模型组的唯一来源；删模型组时经外键从套餐名单移除。

CREATE TABLE plans (
    id INTEGER PRIMARY KEY,
    internal_name TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    note_visible_to_admin INTEGER NOT NULL DEFAULT 0,
    discount_bp INTEGER NOT NULL DEFAULT 10000,
    default_rpm INTEGER,
    shared_rpm INTEGER,
    initial_grant_usd_micros INTEGER NOT NULL DEFAULT 0,
    capabilities_json TEXT NOT NULL,
    shared_with_admin INTEGER NOT NULL DEFAULT 0,
    builtin INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE plan_model_groups (
    plan_id INTEGER NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    group_name TEXT NOT NULL REFERENCES model_groups(name) ON DELETE CASCADE,
    PRIMARY KEY (plan_id, group_name)
) STRICT;

INSERT INTO plans (
    id, internal_name, display_name, note, note_visible_to_admin,
    discount_bp, default_rpm, shared_rpm, initial_grant_usd_micros,
    capabilities_json, shared_with_admin, builtin, created_at
) VALUES
    (1, 'standard', 'Standard', '', 0, 10000, NULL, NULL, 0,
     '{}', 1, 1, 0),
    (2, 'admin', 'Admin', '', 0, 10000, NULL, NULL, 0,
     '{"manage_users":true,"assign_plan":true,"view_logs_stats":true,"settle_waive":true,"toggle_user_tokens":true,"view_own_plan_groups":true}',
     0, 1, 0);

INSERT INTO plan_model_groups (plan_id, group_name) VALUES (1, 'default');

ALTER TABLE users ADD COLUMN plan_id INTEGER REFERENCES plans(id);

-- 存量非 root 用户按其角色挂到对应内置档；root 保持 NULL。
UPDATE users
SET plan_id = CASE
    WHEN id = 1 THEN NULL
    WHEN role = 'admin' THEN 2
    ELSE 1
END;

DROP TABLE user_model_groups;

-- 内置档是固定基础设施：无论应用层还是直接 SQL 都不得删除。
CREATE TRIGGER protect_builtin_plan_delete
BEFORE DELETE ON plans
FOR EACH ROW WHEN OLD.builtin = 1
BEGIN
    SELECT RAISE(ABORT, 'builtin plan cannot be deleted');
END;

-- 内置档 id 是外部契约：改名可以，id 不得变动。
CREATE TRIGGER protect_builtin_plan_id
BEFORE UPDATE OF id ON plans
FOR EACH ROW WHEN OLD.builtin = 1 AND NEW.id != OLD.id
BEGIN
    SELECT RAISE(ABORT, 'builtin plan id cannot change');
END;
