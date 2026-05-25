//! T-213 — C# language extractor.
//!
//! GAP-1: interface/struct/enum/record now use the correct config arrays.
//! GAP-2: namespace_declaration + file_scoped_namespace_declaration push a scope
//!         so qualified names become `Namespace.Class.Method`.

use crate::ast_walker::{DocCommentStyle, LanguageConfig, LanguageExtractor};

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
        assert!(batch.nodes.iter().any(|n| n.name == "Service"), "nodes: {:?}", batch.nodes.iter().map(|n| n.name.as_str()).collect::<Vec<_>>());
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
        let iface = batch.nodes.iter().find(|n| n.name == "IOrderService")
            .expect("IOrderService node should exist");
        assert_eq!(iface.kind, NodeKind::Interface,
            "IOrderService must be Interface, got {:?}", iface.kind);
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
        let en = batch.nodes.iter().find(|n| n.name == "ToastLevel")
            .expect("ToastLevel node should exist");
        assert_eq!(en.kind, NodeKind::Enum,
            "ToastLevel must be Enum, got {:?}", en.kind);
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
        let s = batch.nodes.iter().find(|n| n.name == "Point")
            .expect("Point node should exist");
        assert_eq!(s.kind, NodeKind::Struct,
            "Point must be Struct, got {:?}", s.kind);
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
        let r = batch.nodes.iter().find(|n| n.name == "CatalogItemDetails")
            .expect("CatalogItemDetails node should exist");
        assert_eq!(r.kind, NodeKind::Struct,
            "CatalogItemDetails record must be Struct, got {:?}", r.kind);
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
        let cls = batch.nodes.iter().find(|n| n.name == "BasketService")
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
        let import_refs: Vec<_> = batch.unresolved_refs.iter()
            .filter(|r| r.reference_kind == "imports")
            .collect();
        assert!(
            import_refs.len() >= 2,
            "Expected at least 2 import refs, got {}: {:?}",
            import_refs.len(),
            import_refs.iter().map(|r| r.reference_name.as_str()).collect::<Vec<_>>()
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
        let method = batch.nodes.iter().find(|n| n.name == "CreateOrderAsync")
            .expect("CreateOrderAsync should exist");
        // Signature should be non-None (contains parameters)
        assert!(method.signature.is_some(),
            "CreateOrderAsync should have a signature");
        // metadata should encode is_async
        let meta = method.metadata.as_deref().unwrap_or("{}");
        assert!(meta.contains("\"is_async\":true"),
            "CreateOrderAsync metadata should have is_async:true, got: {meta}");
    }
}
