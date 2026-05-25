//! T-213 — C# language extractor.
//!
//! GAP-1: interface/struct/enum/record now use the correct config arrays.
//! GAP-2: namespace_declaration + file_scoped_namespace_declaration push a scope
//!         so qualified names become `Namespace.Class.Method`.

use crate::ast_walker::{
    generate_node_id, DocCommentStyle, ExtractCtx, LanguageConfig, LanguageExtractor,
};
use codewiki_core::{NodeKind, UnresolvedRef};

pub struct CSharpExtractor;

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
                }
            }
            return false; // let extract_class emit the class node + recurse
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
