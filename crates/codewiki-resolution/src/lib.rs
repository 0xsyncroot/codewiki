// codewiki-resolution — reference resolution pipeline for CodeWiki.
//
// Implements A4 tasks T-301 through T-311:
//   - ImportResolver (T-301 / T-304)
//   - NameMatcher (T-302 / T-305)
//   - PathAliasMap (T-303)
//   - StripComments (T-304 / T-309)
//   - ResolverCaches (T-305 / T-302)
//   - FrameworkResolver trait + registry (T-306)
//   - Framework resolvers: express, react, django, flask, fastapi, rails,
//     laravel, nestjs, spring, gin/go, csharp, swift/vapor, svelte, vue,
//     cargo_workspace (T-307 / T-308)
//   - ReferenceResolver (T-310 / T-309 rename from T-310 in plan)
//   - ResolutionBatchRunner (T-311 / T-310 in plan)

pub mod batch;
pub mod caches;
pub mod framework;
pub mod import_resolver;
pub mod name_matcher;
pub mod path_aliases;
pub mod resolver;
pub mod strip_comments;
pub mod structural_interface;

// Re-exports
pub use batch::{ResolutionBatchRunner, BATCH_SIZE_FULL_INDEX, BATCH_SIZE_INCREMENTAL};
pub use caches::ResolverCaches;
pub use framework::{
    FrameworkExtractionResult, FrameworkResolver, FrameworkResolverRegistry, ResolutionContext,
};
pub use import_resolver::ImportResolver;
pub use name_matcher::NameMatcher;
pub use path_aliases::PathAliasMap;
pub use resolver::ReferenceResolver;
pub use strip_comments::{strip_comments, CommentLang};
pub use structural_interface::{
    synthesize_from_nodes, StructuralInterfaceIndex, STRUCTURAL_GO_PROVENANCE,
};
