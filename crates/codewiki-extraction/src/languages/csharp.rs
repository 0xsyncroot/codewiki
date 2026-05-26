//! T-213 — C# language extractor.
//!
//! GAP-1: interface/struct/enum/record now use the correct config arrays.
//! GAP-2: namespace_declaration + file_scoped_namespace_declaration push a scope
//!         so qualified names become `Namespace.Class.Method`.

use crate::ast_walker::{
    generate_node_id, DocCommentStyle, ExtractCtx, LanguageConfig, LanguageExtractor,
};
use codewiki_core::{NodeKind, UnresolvedRef};
use std::collections::HashSet;

pub struct CSharpExtractor;

/// C# built-in / primitive type names and ubiquitous BCL wrappers that carry no
/// useful blast-radius signal. Type-usage `references` edges to these are
/// dropped so impact/blast over a real domain type isn't drowned in `string` /
/// `int` / `Task` noise.
///
/// These appear in the grammar as `predefined_type` (string, int, …) OR as
/// plain `identifier`s (var, dynamic, Task, …); we filter by bare name so both
/// representations are covered. Kept deliberately conservative: only names that
/// are language keywords or universal BCL primitives, never domain types.
fn is_csharp_builtin_type(name: &str) -> bool {
    matches!(
        name,
        // C# predefined value/reference types & keywords.
        "bool"
            | "byte"
            | "sbyte"
            | "char"
            | "decimal"
            | "double"
            | "float"
            | "int"
            | "uint"
            | "long"
            | "ulong"
            | "short"
            | "ushort"
            | "nint"
            | "nuint"
            | "string"
            | "object"
            | "void"
            | "var"
            | "dynamic"
            | "unmanaged"
            | "delegate"
            // Universal BCL wrappers / aliases that are effectively primitives.
            | "Boolean"
            | "Byte"
            | "SByte"
            | "Char"
            | "Decimal"
            | "Double"
            | "Single"
            | "Int16"
            | "Int32"
            | "Int64"
            | "UInt16"
            | "UInt32"
            | "UInt64"
            | "IntPtr"
            | "UIntPtr"
            | "String"
            | "Object"
            | "Task"
            | "ValueTask"
            | "Void"
    )
}

/// Strip generic brackets and namespace qualifiers from a raw type token,
/// yielding the bare type name (`App.IRepo<T>` → `IRepo`).
fn bare_type_name(raw: &str) -> &str {
    let bare = raw.split('<').next().unwrap_or(raw).trim();
    let bare = bare.rsplit('.').next().unwrap_or(bare).trim();
    // Drop nullable `?` / array `[]` suffixes that may ride along.
    bare.trim_end_matches('?')
        .trim_end_matches("[]")
        .trim_end_matches('?')
        .trim()
}

/// Recursively harvest bare type names referenced by a "type" subtree.
///
/// Handles the C# type grammar: `identifier`, `qualified_name`, `generic_name`
/// (yields BOTH the generic base AND every type argument — e.g. `IRepo<Order>`
/// → `IRepo` + `Order`), `array_type`, `nullable_type`, `pointer_type`,
/// `tuple_type`, and `type_argument_list`. `predefined_type` (string/int/…) and
/// built-in names are skipped. Results are inserted into `out` (de-duplicated by
/// the caller's `HashSet`).
fn collect_type_names(node: &tree_sitter::Node, src: &[u8], out: &mut HashSet<String>) {
    match node.kind() {
        // A leaf type name: `Order`, or a dotted `App.Order` (last segment wins).
        "identifier" | "qualified_name" => {
            let raw = node.utf8_text(src).unwrap_or("").trim();
            let bare = bare_type_name(raw);
            if !bare.is_empty() && !is_csharp_builtin_type(bare) {
                out.insert(bare.to_string());
            }
        }
        // Built-in primitives are intentionally dropped.
        "predefined_type" => {}
        // `IRepo<Order>` → base identifier + recurse into the argument list.
        "generic_name" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_type_names(&child, src, out);
            }
        }
        // Wrappers around an inner element/type: recurse into children.
        "array_type" | "nullable_type" | "pointer_type" | "tuple_type" | "type_argument_list"
        | "ref_type" | "scoped_type" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_type_names(&child, src, out);
            }
        }
        _ => {
            // Unknown wrapper node: recurse so nested types are not missed, but
            // do not treat the node's own text as a type.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_type_names(&child, src, out);
            }
        }
    }
}

/// Identify the "type" child of a member declaration.
///
/// For `field_declaration` the type lives inside `variable_declaration`; for
/// `property_declaration` / `method_declaration` / `parameter` it is the first
/// type-shaped child preceding the member name. Returns the node whose subtree
/// `collect_type_names` should walk.
fn member_type_node<'a>(member: &tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
    // `type:` field is present on parameters and some members in this grammar.
    if let Some(t) = member.child_by_field_name("type") {
        return Some(t);
    }
    // Fallback: first child that looks like a type expression.
    let mut cursor = member.walk();
    let found = member
        .named_children(&mut cursor)
        .find(|child| is_type_node(child));
    found
}

/// True if `node` is one of the type-expression node kinds we harvest.
fn is_type_node(node: &tree_sitter::Node) -> bool {
    matches!(
        node.kind(),
        "identifier"
            | "qualified_name"
            | "generic_name"
            | "predefined_type"
            | "array_type"
            | "nullable_type"
            | "pointer_type"
            | "tuple_type"
            | "ref_type"
            | "scoped_type"
    )
}

/// Collect every distinct, non-built-in type name USED by the members of a type
/// declaration's `declaration_list` body: field types, property types, method
/// return + parameter types, constructor parameter types, and the generic type
/// arguments of all of those.
///
/// De-duplication is per type declaration (one ref per distinct type), so a
/// repository class that uses `Order` in ten signatures yields a single
/// `references` edge to `Order`.
fn collect_member_type_usages(body: &tree_sitter::Node, src: &[u8], out: &mut HashSet<String>) {
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        match member.kind() {
            "field_declaration" => {
                // field_declaration → variable_declaration → <type> declarator…
                let mut fc = member.walk();
                for child in member.named_children(&mut fc) {
                    if child.kind() == "variable_declaration" {
                        if let Some(t) = member_type_node(&child) {
                            collect_type_names(&t, src, out);
                        }
                    }
                }
            }
            "property_declaration"
            | "indexer_declaration"
            | "event_declaration"
            | "event_field_declaration" => {
                if let Some(t) = member_type_node(&member) {
                    collect_type_names(&t, src, out);
                }
            }
            "method_declaration" => {
                // Return type: first type-shaped child before the name.
                if let Some(t) = member_type_node(&member) {
                    collect_type_names(&t, src, out);
                }
                collect_parameter_types(&member, src, out);
            }
            "constructor_declaration" => {
                collect_parameter_types(&member, src, out);
            }
            _ => {}
        }
    }
}

/// Emit one `references` unresolved-ref per distinct non-built-in type USED by
/// the body of a type declaration. The source of every ref is the enclosing
/// type node (`from_id`), so blast/impact over the referenced type reaches the
/// using type. Deduplicated per declaration.
fn emit_type_usage_refs(
    node: &tree_sitter::Node,
    ctx: &mut ExtractCtx,
    type_name: &str,
    kind: &NodeKind,
    from_id: &str,
) {
    let src = ctx.source.as_bytes();
    // The member body is the (last) `declaration_list` child. (A class also has
    // an outer `declaration_list` only at namespace level — here `node` is the
    // declaration itself, whose own `declaration_list` child is the body.)
    let mut body: Option<tree_sitter::Node> = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "declaration_list" {
            body = Some(child);
        }
    }
    let body = match body {
        Some(b) => b,
        None => return,
    };

    let mut types: HashSet<String> = HashSet::new();
    collect_member_type_usages(&body, src, &mut types);
    // Never reference oneself.
    types.remove(type_name);

    let line = node.start_position().row as u32 + 1;
    // Stable, deterministic ordering so node ids don't churn across runs.
    let mut sorted: Vec<String> = types.into_iter().collect();
    sorted.sort_unstable();
    for ty in sorted {
        let ref_id = generate_node_id(
            ctx.file_path,
            kind,
            &format!("references:{type_name}:{ty}"),
            line,
        );
        ctx.unresolved.push(UnresolvedRef {
            id: ref_id,
            from_node_id: from_id.to_string(),
            reference_name: ty,
            reference_kind: "references".to_string(),
            file_path: ctx.file_path.to_string(),
            line: Some(line),
            col: None,
            metadata: None,
        });
    }
}

/// Harvest the types of every `parameter` in a member's `parameter_list`.
fn collect_parameter_types(member: &tree_sitter::Node, src: &[u8], out: &mut HashSet<String>) {
    let mut cursor = member.walk();
    for child in member.named_children(&mut cursor) {
        if child.kind() != "parameter_list" {
            continue;
        }
        let mut pc = child.walk();
        for param in child.named_children(&mut pc) {
            if param.kind() == "parameter" {
                if let Some(t) = member_type_node(&param) {
                    collect_type_names(&t, src, out);
                }
            }
        }
    }
}

static CONFIG: LanguageConfig = LanguageConfig {
    function_types: &[],
    // GAP-1 fix: only class_declaration stays here.
    class_types: &["class_declaration"],
    method_types: &["method_declaration", "constructor_declaration"],
    // GAP-1 fix: each kind now routes to the correct extractor.
    interface_types: &["interface_declaration"],
    // GAP-1 fix: record_declaration maps to NodeKind::Struct (closest semantic fit).
    struct_types: &["struct_declaration", "record_declaration"],
    enum_types: &["enum_declaration"],
    enum_member_types: &["enum_member_declaration"],
    type_alias_types: &[],
    import_types: &["using_directive"],
    call_types: &["invocation_expression"],
    variable_types: &["local_declaration_statement"],
    property_types: &["property_declaration"],
    field_types: &["field_declaration"],
    extra_class_types: &[],
    // GAP-2 fix: push namespace name onto scope stack so qualified_name becomes
    // `MyApp.Namespace::ClassName::MethodName`.
    namespace_types: &["namespace_declaration", "file_scoped_namespace_declaration"],
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::PrecedingLineComment {
        node_kind: "comment",
        prefix: "///",
    },
};

impl LanguageExtractor for CSharpExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }

    fn visit_node_hook(&self, node: &tree_sitter::Node, ctx: &mut ExtractCtx) -> bool {
        // C-CS-3 / D1: `class C : Base, IFoo` → implements refs.
        //
        // C# syntax can't distinguish a base class from interfaces in the
        // base_list, so we emit `implements` for every entry (eShopOnWeb-style
        // `: IFoo` is overwhelmingly interfaces, matching QC expectations).
        //
        // Return `false` so `extract_class` still emits the class node + recurses.
        // The class node id is deterministic, so it can be pre-computed here.
        // D6: when there is no base_list we emit nothing (no spurious edges).
        if node.kind() == "class_declaration" {
            let src = ctx.source.as_bytes();
            if let Some(name_node) = node.child_by_field_name("name") {
                let cname = name_node.utf8_text(src).unwrap_or("").to_string();
                if !cname.is_empty() {
                    let line = node.start_position().row as u32 + 1;
                    let from_id = generate_node_id(ctx.file_path, &NodeKind::Class, &cname, line);

                    // base_list is a (non-field) child of class_declaration.
                    let mut cursor = node.walk();
                    for child in node.named_children(&mut cursor) {
                        if child.kind() != "base_list" {
                            continue;
                        }
                        let mut bc = child.walk();
                        for entry in child.named_children(&mut bc) {
                            // base_list children are the base type names. In this
                            // grammar a simple base is an `identifier`; generic /
                            // qualified bases appear as `generic_name` /
                            // `qualified_name`; an explicit `type` wrapper or a
                            // `primary_constructor_base_type` may also occur. The
                            // only non-type child is `argument_list` (primary-ctor
                            // call args), which we skip.
                            let ty = match entry.kind() {
                                "argument_list" => None,
                                "primary_constructor_base_type" => {
                                    entry.child_by_field_name("type").or(Some(entry))
                                }
                                _ => Some(entry),
                            };
                            if let Some(ty) = ty {
                                let raw = ty.utf8_text(src).unwrap_or("").trim();
                                // Strip generic args: `IRepo<T>` → `IRepo`; keep the
                                // trailing dotted segment: `App.IFoo` → `IFoo`.
                                let bare = raw.split('<').next().unwrap_or(raw).trim();
                                let bare = bare.rsplit('.').next().unwrap_or(bare).trim();
                                // A primary_constructor_base_type's text may include
                                // an argument list `Base(x)`; cut at '('.
                                let bare = bare.split('(').next().unwrap_or(bare).trim();
                                if !bare.is_empty() {
                                    let bline = ty.start_position().row as u32 + 1;
                                    let ref_id = generate_node_id(
                                        ctx.file_path,
                                        &NodeKind::Class,
                                        &format!("implements:{cname}:{bare}"),
                                        bline,
                                    );
                                    ctx.unresolved.push(UnresolvedRef {
                                        id: ref_id,
                                        from_node_id: from_id.clone(),
                                        reference_name: bare.to_string(),
                                        reference_kind: "implements".to_string(),
                                        file_path: ctx.file_path.to_string(),
                                        line: Some(bline),
                                        col: None,
                                        metadata: None,
                                    });
                                }
                            }
                        }
                    }

                    // Type-usage `references` edges (recall fix): emit one
                    // `references` ref per distinct, non-built-in type the class
                    // body USES via field/property/method/ctor signatures and
                    // their generic arguments. Without these, blast/impact over a
                    // widely-used type (e.g. a repository class referenced only
                    // through fields/generics) is near-empty.
                    emit_type_usage_refs(node, ctx, &cname, &NodeKind::Class, &from_id);
                }
            }
            return false; // let extract_class emit the class node + recurse
        }

        // Type-usage `references` for the remaining declaration kinds. Each
        // routes to its own extractor (struct/record→Struct, interface→Interface,
        // enum→Enum) which still runs because we return `false`.
        let decl_kind = match node.kind() {
            "struct_declaration" | "record_declaration" => Some(NodeKind::Struct),
            "interface_declaration" => Some(NodeKind::Interface),
            _ => None,
        };
        if let Some(kind) = decl_kind {
            let src = ctx.source.as_bytes();
            if let Some(name_node) = node.child_by_field_name("name") {
                let tname = name_node.utf8_text(src).unwrap_or("").to_string();
                if !tname.is_empty() {
                    let line = node.start_position().row as u32 + 1;
                    let from_id = generate_node_id(ctx.file_path, &kind, &tname, line);
                    emit_type_usage_refs(node, ctx, &tname, &kind, &from_id);
                }
            }
            return false; // let the per-kind extractor emit the node + recurse
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use crate::ast_walker::extract_file;
    use codewiki_core::NodeKind;
    use std::io::Write;

    #[test]
    fn extract_csharp_class() {
        let source = r#"
namespace MyApp {
    public class Service {
        public void Execute() {}
    }
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".cs").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        assert!(
            batch.nodes.iter().any(|n| n.name == "Service"),
            "nodes: {:?}",
            batch
                .nodes
                .iter()
                .map(|n| n.name.as_str())
                .collect::<Vec<_>>()
        );
    }

    // GAP-1: interface_declaration must emit NodeKind::Interface, not Class.
    #[test]
    fn extract_csharp_interface_kind() {
        let source = r#"
namespace MyApp.Services {
    public interface IOrderService {
        void PlaceOrder();
    }
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".cs").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let iface = batch
            .nodes
            .iter()
            .find(|n| n.name == "IOrderService")
            .expect("IOrderService node should exist");
        assert_eq!(
            iface.kind,
            NodeKind::Interface,
            "IOrderService must be Interface, got {:?}",
            iface.kind
        );
    }

    // GAP-1: enum_declaration must emit NodeKind::Enum.
    #[test]
    fn extract_csharp_enum_kind() {
        let source = r#"
namespace MyApp {
    public enum ToastLevel { Info, Warning, Error }
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".cs").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let en = batch
            .nodes
            .iter()
            .find(|n| n.name == "ToastLevel")
            .expect("ToastLevel node should exist");
        assert_eq!(
            en.kind,
            NodeKind::Enum,
            "ToastLevel must be Enum, got {:?}",
            en.kind
        );
    }

    // GAP-1: struct_declaration must emit NodeKind::Struct.
    #[test]
    fn extract_csharp_struct_kind() {
        let source = r#"
namespace MyApp {
    public struct Point { public int X; public int Y; }
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".cs").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let s = batch
            .nodes
            .iter()
            .find(|n| n.name == "Point")
            .expect("Point node should exist");
        assert_eq!(
            s.kind,
            NodeKind::Struct,
            "Point must be Struct, got {:?}",
            s.kind
        );
    }

    // GAP-1: record_declaration must emit NodeKind::Struct.
    #[test]
    fn extract_csharp_record_kind() {
        let source = r#"
namespace MyApp {
    public record CatalogItemDetails(int Id, string Name);
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".cs").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let r = batch
            .nodes
            .iter()
            .find(|n| n.name == "CatalogItemDetails")
            .expect("CatalogItemDetails node should exist");
        assert_eq!(
            r.kind,
            NodeKind::Struct,
            "CatalogItemDetails record must be Struct, got {:?}",
            r.kind
        );
    }

    // GAP-2: namespace scope must be pushed so qualified_name is namespace-prefixed.
    #[test]
    fn extract_csharp_namespace_qualified_name() {
        let source = r#"
namespace MyApp.Services {
    public class BasketService {
        public void AddItem() {}
    }
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".cs").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let cls = batch
            .nodes
            .iter()
            .find(|n| n.name == "BasketService")
            .expect("BasketService node should exist");
        assert!(
            cls.qualified_name.contains("MyApp.Services"),
            "BasketService qualified_name should contain namespace, got: {}",
            cls.qualified_name
        );
    }

    // GAP-4: using directives must produce import unresolved refs.
    #[test]
    fn extract_csharp_using_produces_imports() {
        let source = r#"
using System;
using Microsoft.AspNetCore.Mvc;
namespace MyApp {
    public class Foo {}
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".cs").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let import_refs: Vec<_> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "imports")
            .collect();
        assert!(
            import_refs.len() >= 2,
            "Expected at least 2 import refs, got {}: {:?}",
            import_refs.len(),
            import_refs
                .iter()
                .map(|r| r.reference_name.as_str())
                .collect::<Vec<_>>()
        );
    }

    // GAP-5: async methods should have is_async=true in metadata and non-None signature.
    #[test]
    fn extract_csharp_async_method_metadata() {
        let source = r#"
namespace MyApp {
    public class OrderService {
        public async Task<Order> CreateOrderAsync(int customerId, string address) {
            return new Order();
        }
    }
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".cs").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let method = batch
            .nodes
            .iter()
            .find(|n| n.name == "CreateOrderAsync")
            .expect("CreateOrderAsync should exist");
        // Signature should be non-None (contains parameters)
        assert!(
            method.signature.is_some(),
            "CreateOrderAsync should have a signature"
        );
        // metadata should encode is_async
        let meta = method.metadata.as_deref().unwrap_or("{}");
        assert!(
            meta.contains("\"is_async\":true"),
            "CreateOrderAsync metadata should have is_async:true, got: {meta}"
        );
    }

    // C-CS-3 / D1: `class Greeter : IGreeter` must emit an implements ref.
    #[test]
    fn extract_csharp_class_implements() {
        let source = r#"
namespace App
{
    public interface IGreeter
    {
        string Greet();
    }

    public class Greeter : IGreeter
    {
        public string Greet()
        {
            return "Hello";
        }
    }

    public class Service
    {
        public string Run()
        {
            return "x";
        }
    }
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".cs").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let impls: Vec<&str> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "implements")
            .map(|r| r.reference_name.as_str())
            .collect();
        assert_eq!(
            impls,
            vec!["IGreeter"],
            "Greeter should implement IGreeter exactly once; impls: {impls:?}"
        );
    }

    // ── Type-usage `references` edges (recall fix) ────────────────────────────

    /// Collect the `references` ref names emitted for a single C# source.
    fn references_of(source: &str) -> Vec<String> {
        let mut f = tempfile::NamedTempFile::with_suffix(".cs").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let mut names: Vec<String> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "references")
            .map(|r| r.reference_name.clone())
            .collect();
        names.sort();
        names
    }

    // Field, property, method param/return, ctor param, and generic args all
    // produce `references` refs; built-ins are skipped; the generic
    // `IRepository<Order>` yields BOTH `IRepository` and `Order`.
    #[test]
    fn csharp_type_usage_references_emitted() {
        let source = r#"
namespace App {
    public class OrderService {
        private readonly OrderRepository _repo;
        public IList<Order> Orders { get; set; }
        public Customer FindCustomer(int id, Address addr) { return null; }
        public OrderService(IRepository<Order> repo, ILogger log) { }
    }
}
"#;
        let refs = references_of(source);
        for expected in [
            "OrderRepository",
            "Order",
            "Customer",
            "Address",
            "IRepository",
            "ILogger",
            "IList",
        ] {
            assert!(
                refs.contains(&expected.to_string()),
                "expected reference to {expected}, got {refs:?}"
            );
        }
        // Built-ins must NOT appear.
        for builtin in ["int", "string", "void", "Task", "var"] {
            assert!(
                !refs.contains(&builtin.to_string()),
                "built-in {builtin} must be skipped, got {refs:?}"
            );
        }
    }

    // A type used in many member sites must yield exactly ONE `references` ref
    // (dedup per declaration), not one per use.
    #[test]
    fn csharp_type_usage_references_deduped() {
        let source = r#"
namespace App {
    public class Repo {
        private Order _a;
        private Order _b;
        public Order Get(Order o) { return o; }
        public IList<Order> All { get; set; }
    }
}
"#;
        let refs = references_of(source);
        let order_count = refs.iter().filter(|r| r.as_str() == "Order").count();
        assert_eq!(
            order_count, 1,
            "Order should be referenced exactly once (deduped), got {refs:?}"
        );
    }

    // Struct, record, and interface member signatures also emit `references`.
    #[test]
    fn csharp_type_usage_references_struct_record_interface() {
        let struct_src = r#"
namespace App {
    public struct Holder {
        public Payload Data { get; set; }
    }
}
"#;
        assert!(
            references_of(struct_src).contains(&"Payload".to_string()),
            "struct member type should be referenced"
        );

        let iface_src = r#"
namespace App {
    public interface IService {
        Result Handle(Command cmd);
    }
}
"#;
        let iface_refs = references_of(iface_src);
        assert!(
            iface_refs.contains(&"Result".to_string())
                && iface_refs.contains(&"Command".to_string()),
            "interface method return + param types should be referenced, got {iface_refs:?}"
        );
    }

    // A built-in-only signature must emit no `references` refs at all.
    #[test]
    fn csharp_type_usage_no_refs_for_builtins_only() {
        let source = r#"
namespace App {
    public class Calc {
        private int _x;
        public string Name { get; set; }
        public double Add(int a, int b) { return 0; }
    }
}
"#;
        assert!(
            references_of(source).is_empty(),
            "built-in-only members must emit no references refs"
        );
    }

    // The source of every type-usage ref is the enclosing type node, so blast
    // over the referenced type reaches the using type.
    #[test]
    fn csharp_type_usage_ref_source_is_enclosing_type() {
        let source = r#"
namespace App {
    public class Consumer {
        private Widget _w;
    }
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".cs").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let consumer = batch
            .nodes
            .iter()
            .find(|n| n.name == "Consumer")
            .expect("Consumer node");
        let widget_ref = batch
            .unresolved_refs
            .iter()
            .find(|r| r.reference_kind == "references" && r.reference_name == "Widget")
            .expect("Widget references ref");
        assert_eq!(
            widget_ref.from_node_id, consumer.id,
            "type-usage ref source must be the enclosing type node"
        );
    }

    // D6: a class with no base list must NOT emit any implements ref.
    #[test]
    fn extract_csharp_no_base_no_spurious_implements() {
        let source = r#"
namespace App
{
    public class Plain
    {
        public void Noop() {}
    }
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".cs").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let impls = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "implements")
            .count();
        assert_eq!(impls, 0, "no base list must yield no implements refs");
    }
}
