//! 工具 JSON Schema 清理：把任意 JSON Schema 收敛到目标可接受的子集。
//!
//! 目前只有 Gemini（OpenAPI 3.0 风格子集）一个目标。原则：**不静默丢弃**——
//! 任何被改写或丢弃的语义都会进入 `SanitizedSchema::warnings`，由调用方记录或展示。
//!
//! Gemini 的已知约束（来自公开的报错信息，实现时按此处理）：
//! - 出现 `anyOf` 时它必须是该层唯一的字段（"any_of must be the only field set"）；
//! - 不接受 `$ref` / `$defs`、`additionalProperties`、`propertyNames`、`not`、`if/then/else` 等；
//! - `type` 不能是数组，可空用 `nullable: true`；
//! - 每个 schema 需要明确的 `type`；`array` 需要 `items`；
//! - `enum` 只接受字符串；`format` 仅接受少数取值。

use std::fmt;

use serde_json::{json, Map, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct SchemaWarning {
    /// 形如 `/properties/foo/items` 的位置（根为空串，显示为 `/`）。
    pub path: String,
    pub message: String,
}

impl fmt::Display for SchemaWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = if self.path.is_empty() { "/" } else { &self.path };
        write!(f, "{p}: {}", self.message)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SanitizedSchema {
    /// 清理后的 schema；`None` 表示根本没有可发送的参数（应省略 `parameters` 字段）。
    pub schema: Option<Value>,
    pub warnings: Vec<SchemaWarning>,
}

const MAX_DEPTH: usize = 48;

/// 元数据类关键字：丢弃不影响校验语义，不产生告警。
const SILENT: &[&str] = &["$schema", "$id", "$comment", "$anchor", "$defs", "definitions", "readOnly", "writeOnly", "deprecated"];

const STRING_KEYS: &[&str] = &["format", "pattern", "minLength", "maxLength"];
const NUMBER_KEYS: &[&str] = &["format", "minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum", "multipleOf"];
const ARRAY_KEYS: &[&str] = &["items", "minItems", "maxItems", "uniqueItems", "prefixItems", "contains"];
const OBJECT_KEYS: &[&str] = &["properties", "required", "minProperties", "maxProperties"];

fn applicable(ty: &str) -> Vec<&'static str> {
    let mut v = vec!["type", "title", "description", "nullable", "enum"];
    match ty {
        "string" => v.extend(STRING_KEYS),
        "integer" | "number" => v.extend(NUMBER_KEYS),
        "array" => v.extend(ARRAY_KEYS),
        "object" => v.extend(OBJECT_KEYS),
        _ => {}
    }
    v
}

/// 其它类型才适用的关键字——出现在不匹配的类型上等价于无效果，静默忽略。
fn belongs_to_other_type(key: &str, ty: &str) -> bool {
    [("string", STRING_KEYS), ("number", NUMBER_KEYS), ("array", ARRAY_KEYS), ("object", OBJECT_KEYS)]
        .iter()
        .any(|(t, keys)| {
            let same = *t == ty || (*t == "number" && ty == "integer");
            !same && keys.contains(&key)
        })
}

struct Ctx<'a> {
    root: &'a Value,
    warnings: Vec<SchemaWarning>,
    ref_stack: Vec<String>,
}

fn placeholder(note: &str) -> Value {
    json!({"type": "string", "description": note})
}

fn child_path(path: &str, seg: &str) -> String {
    format!("{path}/{}", seg.replace('~', "~0").replace('/', "~1"))
}

impl Ctx<'_> {
    fn warn(&mut self, path: &str, message: impl Into<String>) {
        self.warnings.push(SchemaWarning { path: path.to_string(), message: message.into() });
    }

    fn resolve_ref(&self, r: &str) -> Option<&Value> {
        let pointer = r.strip_prefix('#')?;
        self.root.pointer(pointer)
    }

    /// 把 `sub`（可能含 $ref / allOf）并入 `base`。
    fn absorb(&mut self, base: &mut Map<String, Value>, sub: &Value, path: &str, depth: usize) {
        let Some(mut sub_obj) = sub.as_object().cloned() else {
            self.warn(path, "allOf member is not an object schema and was ignored");
            return;
        };
        if depth > MAX_DEPTH {
            self.warn(path, "allOf nesting too deep; member ignored");
            return;
        }
        if let Some(Value::String(r)) = sub_obj.remove("$ref") {
            match self.resolve_ref(&r).cloned() {
                Some(target) => {
                    self.absorb(base, &target, path, depth + 1);
                }
                None => self.warn(path, format!("unresolvable $ref `{r}` inside allOf was ignored")),
            }
        }
        if let Some(Value::Array(inner)) = sub_obj.remove("allOf") {
            for i in &inner {
                self.absorb(base, i, path, depth + 1);
            }
        }
        merge_into(base, &sub_obj);
    }

    fn clean(&mut self, node: &Value, path: &str, depth: usize) -> Value {
        if depth > MAX_DEPTH {
            self.warn(path, "schema nesting too deep; replaced by a string placeholder");
            return placeholder("(schema too deeply nested)");
        }
        let Some(mut obj) = node.as_object().cloned() else {
            self.warn(path, "boolean/non-object schema has no type; defaulted to string");
            return json!({"type": "string"});
        };

        // 1) $ref：内联目标，兄弟关键字（如 description）覆盖目标。
        if let Some(Value::String(r)) = obj.remove("$ref") {
            if self.ref_stack.contains(&r) {
                self.warn(path, format!("recursive $ref `{r}` cannot be expressed; replaced by a string placeholder"));
                return placeholder("(recursive structure omitted)");
            }
            let Some(target) = self.resolve_ref(&r).cloned() else {
                self.warn(path, format!("unresolvable $ref `{r}`; replaced by a string placeholder"));
                return placeholder("(unresolved reference)");
            };
            let mut merged = target.as_object().cloned().unwrap_or_default();
            for (k, v) in obj {
                merged.insert(k, v);
            }
            self.ref_stack.push(r);
            let out = self.clean(&Value::Object(merged), path, depth + 1);
            self.ref_stack.pop();
            return out;
        }

        // 2) allOf：合并成单个 schema。
        if let Some(Value::Array(subs)) = obj.remove("allOf") {
            self.warn(path, "allOf was merged into a single schema (conflicting members keep the first definition)");
            let mut base = Map::new();
            for s in &subs {
                self.absorb(&mut base, s, path, depth + 1);
            }
            merge_into(&mut obj, &base);
            // obj 自己的 properties 优先于 allOf 成员；merge_into 对已存在键保持不变。
        }

        // 3) oneOf → anyOf（互斥语义丢失）。
        if let Some(Value::Array(one)) = obj.remove("oneOf") {
            self.warn(path, "oneOf was converted to anyOf (exclusivity is not enforced by Gemini)");
            let any = obj.entry("anyOf").or_insert_with(|| Value::Array(vec![]));
            if let Value::Array(a) = any {
                a.extend(one);
            }
        }

        // 4) type 数组 → anyOf（含 null 时转 nullable）。
        if let Some(Value::Array(types)) = obj.get("type").cloned() {
            let names: Vec<String> = types.iter().filter_map(|t| t.as_str().map(str::to_lowercase)).collect();
            let nullable = names.iter().any(|t| t == "null");
            let non_null: Vec<&String> = names.iter().filter(|t| *t != "null").collect();
            match non_null.len() {
                0 => {
                    obj.insert("type".into(), json!("string"));
                    obj.insert("nullable".into(), json!(true));
                    self.warn(path, "type list contained only `null`; defaulted to nullable string");
                }
                1 => {
                    obj.insert("type".into(), json!(non_null[0]));
                    if nullable {
                        obj.insert("nullable".into(), json!(true));
                    }
                }
                _ => {
                    let mut variants: Vec<Value> = non_null
                        .iter()
                        .map(|t| {
                            let mut copy = obj.clone();
                            copy.insert("type".into(), json!(t));
                            Value::Object(copy)
                        })
                        .collect();
                    if nullable {
                        variants.push(json!({"type": "null"}));
                    }
                    return self.clean(&json!({"anyOf": variants}), path, depth + 1);
                }
            }
        }

        // 5) anyOf：去掉 null 变体 → nullable；Gemini 要求 anyOf 独占该层。
        if let Some(Value::Array(variants)) = obj.remove("anyOf") {
            let is_null = |v: &Value| v.get("type").and_then(Value::as_str) == Some("null");
            let nullable = variants.iter().any(is_null);
            let non_null: Vec<Value> = variants.into_iter().filter(|v| !is_null(v)).collect();
            match non_null.len() {
                0 => {
                    self.warn(path, "anyOf contained only `null`; defaulted to nullable string");
                    return json!({"type": "string", "nullable": true});
                }
                1 => {
                    let mut merged = non_null[0].as_object().cloned().unwrap_or_default();
                    for (k, v) in obj {
                        merged.insert(k, v);
                    }
                    if nullable {
                        merged.insert("nullable".into(), json!(true));
                    }
                    return self.clean(&Value::Object(merged), path, depth + 1);
                }
                _ => {
                    let dropped: Vec<&str> = obj
                        .keys()
                        .map(String::as_str)
                        .filter(|k| !SILENT.contains(k))
                        .collect();
                    if !dropped.is_empty() {
                        self.warn(
                            path,
                            format!(
                                "keywords {} next to anyOf were dropped (Gemini requires anyOf to be the only field)",
                                dropped.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ")
                            ),
                        );
                    }
                    let mut out = Vec::new();
                    for (i, v) in non_null.iter().enumerate() {
                        let mut c = self.clean(v, &child_path(&format!("{path}/anyOf"), &i.to_string()), depth + 1);
                        if nullable {
                            if let Some(o) = c.as_object_mut() {
                                o.insert("nullable".into(), json!(true));
                            }
                        }
                        out.push(c);
                    }
                    return json!({"anyOf": out});
                }
            }
        }

        // 6) const → enum。
        if let Some(c) = obj.remove("const") {
            obj.entry("enum").or_insert_with(|| json!([c]));
        }

        // 7) 推断 type。
        let ty: String = match obj.get("type").and_then(Value::as_str) {
            Some(t) => t.to_lowercase(),
            None => {
                let inferred = if obj.contains_key("properties") {
                    "object"
                } else if obj.contains_key("items") {
                    "array"
                } else {
                    self.warn(path, "schema has no `type`; defaulted to string");
                    "string"
                };
                inferred.to_string()
            }
        };

        let mut out = Map::new();
        let mut ty_out = ty.clone();

        // enum：Gemini 只接受字符串。
        if let Some(Value::Array(vals)) = obj.get("enum") {
            if vals.iter().all(Value::is_string) {
                out.insert("enum".into(), Value::Array(vals.clone()));
            } else {
                self.warn(path, "enum values were converted to strings and type set to string (Gemini only accepts string enums)");
                let strs: Vec<Value> = vals
                    .iter()
                    .map(|v| match v {
                        Value::String(s) => json!(s),
                        other => json!(other.to_string()),
                    })
                    .collect();
                out.insert("enum".into(), Value::Array(strs));
                ty_out = "string".into();
            }
        }

        match ty.as_str() {
            "object" => {
                let mut props_out = Map::new();
                if let Some(Value::Object(props)) = obj.get("properties") {
                    for (k, v) in props {
                        props_out.insert(k.clone(), self.clean(v, &child_path(&format!("{path}/properties"), k), depth + 1));
                    }
                }
                if let Some(Value::Array(req)) = obj.get("required") {
                    let (keep, missing): (Vec<&Value>, Vec<&Value>) =
                        req.iter().partition(|r| r.as_str().is_some_and(|s| props_out.contains_key(s)));
                    if !missing.is_empty() {
                        self.warn(path, "required entries without a matching property were dropped");
                    }
                    if !keep.is_empty() {
                        out.insert("required".into(), Value::Array(keep.into_iter().cloned().collect()));
                    }
                }
                if !props_out.is_empty() {
                    out.insert("properties".into(), Value::Object(props_out));
                }
            }
            "array" => {
                let items = match obj.get("items") {
                    Some(Value::Array(tuple)) => {
                        self.warn(path, "tuple-style `items` array is not supported; only the first item schema was kept");
                        tuple.first().cloned()
                    }
                    Some(v) => Some(v.clone()),
                    None => None,
                };
                let items_out = match items {
                    Some(v) => self.clean(&v, &format!("{path}/items"), depth + 1),
                    None => {
                        self.warn(path, "array schema without `items`; defaulted items to string");
                        json!({"type": "string"})
                    }
                };
                out.insert("items".into(), items_out);
            }
            _ => {}
        }

        // 其余标量关键字。
        let allowed = applicable(&ty);
        for (k, v) in &obj {
            match k.as_str() {
                "type" | "enum" | "properties" | "required" | "items" => {}
                "title" | "description" => {
                    out.insert(k.clone(), v.clone());
                }
                "nullable" => {
                    if v == &json!(true) {
                        out.insert(k.clone(), v.clone());
                    }
                }
                "format" => {
                    let f = v.as_str().unwrap_or("");
                    let ok = match ty.as_str() {
                        "string" => matches!(f, "date-time" | "enum"),
                        "integer" => matches!(f, "int32" | "int64"),
                        "number" => matches!(f, "float" | "double"),
                        _ => false,
                    };
                    if ok {
                        out.insert(k.clone(), v.clone());
                    } else if allowed.contains(&"format") {
                        self.warn(path, format!("format `{f}` is not supported for type {ty} and was dropped"));
                    }
                }
                "pattern" | "minLength" | "maxLength" | "minimum" | "maximum" | "minItems" | "maxItems" if allowed.contains(&k.as_str()) => {
                    out.insert(k.clone(), v.clone());
                }
                other if SILENT.contains(&other) => {}
                other if belongs_to_other_type(other, &ty) => {}
                other => {
                    self.warn(path, format!("keyword `{other}` is not supported by Gemini and was dropped"));
                }
            }
        }

        out.insert("type".into(), json!(ty_out));
        Value::Object(out)
    }
}

/// 把 `src` 的键并入 `dst`：`properties` 取并集、`required` 取并集，其余键已存在则保持原值。
fn merge_into(dst: &mut Map<String, Value>, src: &Map<String, Value>) {
    for (k, v) in src {
        match (k.as_str(), dst.get_mut(k)) {
            ("properties", Some(Value::Object(existing))) => {
                if let Value::Object(new) = v {
                    for (pk, pv) in new {
                        existing.entry(pk.clone()).or_insert_with(|| pv.clone());
                    }
                }
            }
            ("required", Some(Value::Array(existing))) => {
                if let Value::Array(new) = v {
                    for r in new {
                        if !existing.contains(r) {
                            existing.push(r.clone());
                        }
                    }
                }
            }
            (_, Some(_)) => {}
            (_, None) => {
                dst.insert(k.clone(), v.clone());
            }
        }
    }
}

/// 把工具的 `input_schema` 清理成 Gemini `functionDeclarations[].parameters` 可接受的子集。
pub fn sanitize_for_gemini(schema: &Value) -> SanitizedSchema {
    let mut ctx = Ctx { root: schema, warnings: Vec::new(), ref_stack: Vec::new() };
    let mut cleaned = ctx.clean(schema, "", 0);

    // 根上的 anyOf 无法同时满足「anyOf 独占」与「根必须是 object」：尝试合并对象变体。
    if let Some(Value::Array(variants)) = cleaned.get("anyOf").cloned() {
        let all_objects = variants.iter().all(|v| v.get("type").and_then(Value::as_str) == Some("object"));
        if all_objects && !variants.is_empty() {
            ctx.warn("", "root-level anyOf of objects was merged into one object (union of properties, intersection of required)");
            let mut props = Map::new();
            for v in &variants {
                if let Some(Value::Object(p)) = v.get("properties") {
                    for (k, pv) in p {
                        props.entry(k.clone()).or_insert_with(|| pv.clone());
                    }
                }
            }
            let mut required: Option<Vec<Value>> = None;
            for v in &variants {
                let r: Vec<Value> = v.get("required").and_then(Value::as_array).cloned().unwrap_or_default();
                required = Some(match required {
                    None => r,
                    Some(prev) => prev.into_iter().filter(|x| r.contains(x)).collect(),
                });
            }
            let mut merged = Map::new();
            merged.insert("type".into(), json!("object"));
            if !props.is_empty() {
                merged.insert("properties".into(), Value::Object(props));
            }
            if let Some(r) = required.filter(|r| !r.is_empty()) {
                merged.insert("required".into(), Value::Array(r));
            }
            cleaned = Value::Object(merged);
        } else {
            ctx.warn("", "root-level anyOf with non-object variants cannot be expressed; parameters were omitted");
            return SanitizedSchema { schema: None, warnings: ctx.warnings };
        }
    }

    if cleaned.get("type").and_then(Value::as_str) != Some("object") {
        ctx.warn("", "tool parameters root must be an object schema; parameters were omitted");
        return SanitizedSchema { schema: None, warnings: ctx.warnings };
    }
    let has_props = cleaned.get("properties").and_then(Value::as_object).is_some_and(|p| !p.is_empty());
    let schema = if has_props { Some(cleaned) } else { None };
    SanitizedSchema { schema, warnings: ctx.warnings }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(schema: Value) -> SanitizedSchema {
        sanitize_for_gemini(&schema)
    }

    fn has_warning(s: &SanitizedSchema, needle: &str) -> bool {
        s.warnings.iter().any(|w| w.message.contains(needle))
    }

    #[test]
    fn plain_schema_passes_through_without_warnings() {
        let s = run(json!({
            "type": "object",
            "properties": {
                "city": {"type": "string", "description": "City name"},
                "days": {"type": "integer", "minimum": 1, "maximum": 7},
                "units": {"type": "string", "enum": ["c", "f"]},
                "tags": {"type": "array", "items": {"type": "string"}, "maxItems": 3}
            },
            "required": ["city"]
        }));
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        let schema = s.schema.clone().unwrap();
        assert_eq!(schema["required"], json!(["city"]));
        assert_eq!(schema["properties"]["days"], json!({"type": "integer", "minimum": 1, "maximum": 7}));
        assert_eq!(schema["properties"]["tags"]["items"], json!({"type": "string"}));
    }

    #[test]
    fn type_array_with_null_becomes_nullable() {
        let s = run(json!({"type": "object", "properties": {"a": {"type": ["string", "null"]}}}));
        assert_eq!(s.schema.clone().unwrap()["properties"]["a"], json!({"type": "string", "nullable": true}));
        assert!(s.warnings.is_empty());
    }

    #[test]
    fn multi_type_array_becomes_any_of() {
        let s = run(json!({"type": "object", "properties": {"a": {"type": ["string", "integer", "null"]}}}));
        assert_eq!(
            s.schema.clone().unwrap()["properties"]["a"],
            json!({"anyOf": [{"type": "string", "nullable": true}, {"type": "integer", "nullable": true}]})
        );
    }

    #[test]
    fn any_of_with_null_collapses_to_nullable_and_keeps_description() {
        let s = run(json!({"type": "object", "properties": {
            "a": {"description": "maybe text", "anyOf": [{"type": "string"}, {"type": "null"}]}
        }}));
        assert_eq!(
            s.schema.clone().unwrap()["properties"]["a"],
            json!({"type": "string", "nullable": true, "description": "maybe text"})
        );
        assert!(s.warnings.is_empty());
    }

    #[test]
    fn any_of_must_be_only_field_and_siblings_are_reported() {
        let s = run(json!({"type": "object", "properties": {
            "a": {"description": "one or the other", "anyOf": [{"type": "string"}, {"type": "integer"}]}
        }}));
        assert_eq!(
            s.schema.clone().unwrap()["properties"]["a"],
            json!({"anyOf": [{"type": "string"}, {"type": "integer"}]})
        );
        assert!(has_warning(&s, "`description`"));
        assert_eq!(s.warnings[0].path, "/properties/a");
    }

    #[test]
    fn one_of_is_converted_with_warning() {
        let s = run(json!({"type": "object", "properties": {
            "a": {"oneOf": [{"type": "string"}, {"type": "number"}]}
        }}));
        assert_eq!(s.schema.clone().unwrap()["properties"]["a"], json!({"anyOf": [{"type": "string"}, {"type": "number"}]}));
        assert!(has_warning(&s, "oneOf"));
    }

    #[test]
    fn refs_are_inlined() {
        let s = run(json!({
            "type": "object",
            "properties": {"addr": {"$ref": "#/$defs/Address", "description": "Where"}},
            "$defs": {"Address": {"type": "object", "properties": {"zip": {"type": "string"}}, "required": ["zip"]}}
        }));
        let schema = s.schema.clone().unwrap();
        assert_eq!(
            schema["properties"]["addr"],
            json!({"type": "object", "properties": {"zip": {"type": "string"}}, "required": ["zip"], "description": "Where"})
        );
        assert!(schema.get("$defs").is_none());
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    }

    #[test]
    fn recursive_refs_are_cut_with_warning() {
        let s = run(json!({
            "type": "object",
            "properties": {"tree": {"$ref": "#/$defs/Node"}},
            "$defs": {"Node": {"type": "object", "properties": {
                "value": {"type": "string"},
                "child": {"$ref": "#/$defs/Node"}
            }}}
        }));
        let schema = s.schema.clone().unwrap();
        assert_eq!(schema["properties"]["tree"]["properties"]["child"]["type"], "string");
        assert!(has_warning(&s, "recursive $ref"));
    }

    #[test]
    fn unresolvable_and_external_refs_warn() {
        let s = run(json!({"type": "object", "properties": {
            "a": {"$ref": "#/$defs/Missing"},
            "b": {"$ref": "https://example.com/schema.json"}
        }}));
        assert_eq!(s.warnings.iter().filter(|w| w.message.contains("unresolvable")).count(), 2);
    }

    #[test]
    fn additional_properties_and_unsupported_keywords_are_reported_not_silent() {
        let s = run(json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "u": {"type": "string", "format": "uri"},
                "n": {"type": "number", "exclusiveMinimum": 0},
                "m": {"type": "object", "properties": {"x": {"type": "string"}}, "propertyNames": {"pattern": "^a"}},
                "d": {"type": "string", "default": "x"}
            }
        }));
        let schema = s.schema.clone().unwrap();
        assert!(schema.get("additionalProperties").is_none());
        assert!(schema["properties"]["u"].get("format").is_none());
        assert!(has_warning(&s, "`additionalProperties`"));
        assert!(has_warning(&s, "format `uri`"));
        assert!(has_warning(&s, "`exclusiveMinimum`"));
        assert!(has_warning(&s, "`propertyNames`"));
        assert!(has_warning(&s, "`default`"));
        // $schema 是元数据，静默
        assert!(!has_warning(&s, "$schema"));
    }

    #[test]
    fn supported_formats_are_kept() {
        let s = run(json!({"type": "object", "properties": {
            "t": {"type": "string", "format": "date-time"},
            "i": {"type": "integer", "format": "int64"}
        }}));
        let p = s.schema.clone().unwrap();
        assert_eq!(p["properties"]["t"]["format"], "date-time");
        assert_eq!(p["properties"]["i"]["format"], "int64");
        assert!(s.warnings.is_empty());
    }

    #[test]
    fn all_of_is_merged() {
        let s = run(json!({"type": "object", "properties": {
            "a": {"allOf": [
                {"type": "object", "properties": {"x": {"type": "string"}}, "required": ["x"]},
                {"properties": {"y": {"type": "integer"}}, "required": ["y"]}
            ]}
        }}));
        let a = &s.schema.clone().unwrap()["properties"]["a"];
        assert_eq!(a["type"], "object");
        assert_eq!(a["properties"]["x"]["type"], "string");
        assert_eq!(a["properties"]["y"]["type"], "integer");
        assert_eq!(a["required"], json!(["x", "y"]));
        assert!(has_warning(&s, "allOf"));
    }

    #[test]
    fn const_and_non_string_enums() {
        let s = run(json!({"type": "object", "properties": {
            "c": {"const": "fixed"},
            "e": {"type": "integer", "enum": [1, 2, 3]}
        }}));
        let p = s.schema.clone().unwrap();
        assert_eq!(p["properties"]["c"], json!({"type": "string", "enum": ["fixed"]}));
        assert_eq!(p["properties"]["e"], json!({"type": "string", "enum": ["1", "2", "3"]}));
        assert!(has_warning(&s, "enum values were converted"));
    }

    #[test]
    fn required_entries_without_properties_are_dropped_with_warning() {
        let s = run(json!({"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a", "ghost"]}));
        assert_eq!(s.schema.clone().unwrap()["required"], json!(["a"]));
        assert!(has_warning(&s, "required entries"));
    }

    #[test]
    fn arrays_get_items_and_tuple_items_are_reduced() {
        let s = run(json!({"type": "object", "properties": {
            "a": {"type": "array"},
            "b": {"type": "array", "items": [{"type": "string"}, {"type": "number"}]}
        }}));
        let p = s.schema.clone().unwrap();
        assert_eq!(p["properties"]["a"]["items"], json!({"type": "string"}));
        assert_eq!(p["properties"]["b"]["items"], json!({"type": "string"}));
        assert!(has_warning(&s, "without `items`"));
        assert!(has_warning(&s, "tuple-style"));
    }

    #[test]
    fn missing_type_is_inferred_or_defaulted() {
        let s = run(json!({"properties": {"a": {"properties": {"b": {"type": "string"}}}, "c": {}}}));
        let p = s.schema.clone().unwrap();
        assert_eq!(p["type"], "object");
        assert_eq!(p["properties"]["a"]["type"], "object");
        assert_eq!(p["properties"]["c"]["type"], "string");
        assert!(has_warning(&s, "no `type`"));
    }

    #[test]
    fn empty_parameters_are_omitted() {
        assert_eq!(run(json!({"type": "object", "properties": {}})).schema, None);
        assert_eq!(run(json!({"type": "object"})).schema, None);
    }

    #[test]
    fn root_any_of_of_objects_is_merged() {
        let s = run(json!({"anyOf": [
            {"type": "object", "properties": {"a": {"type": "string"}, "k": {"type": "string"}}, "required": ["k", "a"]},
            {"type": "object", "properties": {"b": {"type": "integer"}, "k": {"type": "string"}}, "required": ["k"]}
        ]}));
        let p = s.schema.clone().unwrap();
        assert_eq!(p["type"], "object");
        assert_eq!(p["properties"].as_object().unwrap().len(), 3);
        assert_eq!(p["required"], json!(["k"]));
        assert!(has_warning(&s, "root-level anyOf"));
    }

    #[test]
    fn non_object_root_is_omitted_with_warning() {
        let s = run(json!({"type": "string"}));
        assert_eq!(s.schema, None);
        assert!(has_warning(&s, "root must be an object"));
    }

    #[test]
    fn deep_nesting_is_cut_not_overflowed() {
        let mut v = json!({"type": "string"});
        for _ in 0..80 {
            v = json!({"type": "object", "properties": {"n": v}});
        }
        let s = run(v);
        assert!(has_warning(&s, "nesting too deep"));
    }

    #[test]
    fn warning_display_includes_path() {
        let w = SchemaWarning { path: "/properties/a".into(), message: "boom".into() };
        assert_eq!(w.to_string(), "/properties/a: boom");
        let root = SchemaWarning { path: String::new(), message: "boom".into() };
        assert_eq!(root.to_string(), "/: boom");
    }
}
