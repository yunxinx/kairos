//! 工具输入模式的 object 根归一化。
//!
//! Anthropic 等上游对 tool 输入模式有硬约束：根级必须是显式 object 形态，
//! 且不接受根级 union。归一化作为纯函数供各出站面共用——适配器编码侧按需
//! 调用，请求整流器在上游以 schema 相关 400 拒绝后触发重试。

use serde_json::{Value, json};

/// 归一化 tool 输入模式，满足「根级必须是显式 object 形态且不接受根级
/// union」的上游硬约束。
///
/// 归一化动作（按序应用，全部发生时才产生 action 说明）：
/// - 根级非 JSON 对象（含 schema 缺席）或 type 显式非 object：兜底为空
///   object schema，上游不再因形态非法的 input_schema 拒绝整个请求；
/// - 根级 `anyOf`/`oneOf`/`allOf` 摊平：object 分支的 properties 并入根
///   properties（先到先得，分支间同名冲突不覆盖），`allOf` 分支的 required
///   并入根 required（合取语义，去重）；anyOf/oneOf 是析取，其分支的
///   required 不并入；
/// - 根级 type 缺席或为含 object 的类型列表：置为 `"object"`。
///
/// 已是 object 形态的 schema 原样返回（值相等，序列化逐字节不变）且不产生
/// action；归一化幂等。嵌套层内的 union 不处理。返回归一化结果与发生的
/// 改写动作说明。
pub(crate) fn normalize_object_root(schema: Option<&Value>) -> (Value, Option<String>) {
    /// 可接受的最小 object schema。
    fn empty_object_schema() -> Value {
        json!({ "type": "object", "properties": {} })
    }

    let Some(Value::Object(root)) = schema else {
        return (
            empty_object_schema(),
            Some("非 object 根已兜底为空 object schema".to_string()),
        );
    };
    if !schema_can_be_object(root.get("type")) {
        return (
            empty_object_schema(),
            Some("非 object 根已兜底为空 object schema".to_string()),
        );
    }
    let mut root = root.clone();
    let mut properties = match root.get("properties") {
        Some(Value::Object(map)) => map.clone(),
        _ => serde_json::Map::new(),
    };
    let mut required = required_schema_names(root.get("required")).unwrap_or_default();
    let mut flattened = false;
    for union_name in ["anyOf", "oneOf", "allOf"] {
        let Some(branches) = root.remove(union_name) else {
            continue;
        };
        flattened = true;
        let Value::Array(branches) = branches else {
            continue;
        };
        for branch in branches {
            let Value::Object(branch) = branch else {
                continue;
            };
            if !schema_can_be_object(branch.get("type")) {
                continue;
            }
            if let Some(Value::Object(branch_properties)) = branch.get("properties") {
                for (name, property) in branch_properties {
                    properties
                        .entry(name.clone())
                        .or_insert_with(|| property.clone());
                }
            }
            if union_name == "allOf" {
                for name in required_schema_names(branch.get("required")).unwrap_or_default() {
                    if !required.contains(&name) {
                        required.push(name);
                    }
                }
            }
        }
    }
    root.insert("type".into(), json!("object"));
    root.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        root.insert("required".into(), json!(required));
    }
    let normalized = Value::Object(root);
    if !flattened && schema == Some(&normalized) {
        return (normalized, None);
    }
    let action = if flattened {
        "根级 anyOf/oneOf/allOf 已摊平合并".to_string()
    } else {
        "根级已归一化为显式 object 形态".to_string()
    };
    (normalized, Some(action))
}

/// schema `type` 声明能否承载 object：缺席视为可能，`"object"` 或含
/// `"object"` 的类型列表成立，其余（其他标量、非法形态）不成立。
fn schema_can_be_object(schema_type: Option<&Value>) -> bool {
    match schema_type {
        None => true,
        Some(Value::String(name)) => name == "object",
        Some(Value::Array(names)) => names.iter().any(|name| name.as_str() == Some("object")),
        Some(_) => false,
    }
}

/// 读取 schema `required` 为字符串名列表；缺席、非数组或含非字符串条目时
/// 返回 `None`（视为不可靠来源，不并入）。
fn required_schema_names(required: Option<&Value>) -> Option<Vec<String>> {
    let Value::Array(items) = required? else {
        return None;
    };
    let mut names = Vec::with_capacity(items.len());
    for item in items {
        names.push(item.as_str()?.to_string());
    }
    Some(names)
}

#[cfg(test)]
mod tests {
    use super::normalize_object_root;
    use serde_json::json;

    /// 合法 object schema 原样保留（逐字节不变），根级 union 摊平合并，
    /// 非 object 根兜底空 object schema，归一化幂等。
    #[test]
    fn normalizes_object_root() {
        // 合法 object schema：值不变，无动作说明。
        let plain = json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"],
        });
        let (normalized, action) = normalize_object_root(Some(&plain));
        assert_eq!(normalized, plain);
        assert_eq!(
            serde_json::to_string(&normalized).unwrap(),
            serde_json::to_string(&plain).unwrap()
        );
        assert_eq!(action, None);
        // 幂等：对归一化结果再归一化不再变化。
        let union = json!({
            "anyOf": [
                { "type": "object", "properties": { "a": { "type": "string" } } },
                { "type": "string" },
                { "type": "object", "properties": { "b": { "type": "number" } } },
            ]
        });
        let (once, _) = normalize_object_root(Some(&union));
        let (twice, action) = normalize_object_root(Some(&once));
        assert_eq!(once, twice);
        assert_eq!(action, None);

        // anyOf/oneOf 摊平：object 分支 properties 并入，非 object 分支跳过；
        // 同名属性先到先得。
        let (normalized, action) = normalize_object_root(Some(&union));
        assert_eq!(
            normalized,
            json!({
                "type": "object",
                "properties": {
                    "a": { "type": "string" },
                    "b": { "type": "number" },
                },
            })
        );
        assert_eq!(
            action,
            Some("根级 anyOf/oneOf/allOf 已摊平合并".to_string())
        );

        // allOf 分支 required 并入（合取语义），anyOf 分支 required 不并入；
        // 重复名去重。
        let mixed = json!({
            "type": "object",
            "properties": { "x": { "type": "string" } },
            "anyOf": [
                { "type": "object", "properties": { "y": { "type": "string" } }, "required": ["y"] },
            ],
            "allOf": [
                { "type": "object", "properties": { "z": { "type": "string" } }, "required": ["x", "z"] },
            ],
        });
        let (normalized, action) = normalize_object_root(Some(&mixed));
        assert_eq!(
            normalized,
            json!({
                "type": "object",
                "properties": {
                    "x": { "type": "string" },
                    "y": { "type": "string" },
                    "z": { "type": "string" },
                },
                "required": ["x", "z"],
            })
        );
        assert_eq!(
            action,
            Some("根级 anyOf/oneOf/allOf 已摊平合并".to_string())
        );

        // 非 object 根兜底空 object schema；schema 缺席同。
        for schema in [
            json!("string"),
            json!({ "type": "string" }),
            json!({ "type": ["string", "null"] }),
        ] {
            let (normalized, action) = normalize_object_root(Some(&schema));
            assert_eq!(normalized, json!({ "type": "object", "properties": {} }));
            assert_eq!(
                action,
                Some("非 object 根已兜底为空 object schema".to_string())
            );
        }
        let (normalized, action) = normalize_object_root(None);
        assert_eq!(normalized, json!({ "type": "object", "properties": {} }));
        assert_eq!(
            action,
            Some("非 object 根已兜底为空 object schema".to_string())
        );
    }
}
