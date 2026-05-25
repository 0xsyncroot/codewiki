// T-306 — FrameworkResolver trait + registry
//
// Defines the pluggable framework resolver interface and the registry that
// instantiates all 14+ framework resolvers.

pub mod angular;
pub mod cargo_workspace;
pub mod csharp;
pub(crate) mod scan_utils;
pub mod django;
pub mod express;
pub mod fastapi;
pub mod flask;
pub mod go;
pub mod laravel;
pub mod nestjs;
pub mod rails;
pub mod react;
pub mod spring;
pub mod svelte;
pub mod swift;
pub mod vue;

use crate::caches::ResolverCaches;
use crate::path_aliases::PathAliasMap;
use codewiki_core::{CodeWikiError, Edge, Language, Node, UnresolvedRef};
use codewiki_storage::traits::{ResolvedBy, ResolvedEdge, ResolvedFromRef};
use std::path::Path;
use std::sync::Arc;

// ─── Context ──────────────────────────────────────────────────────────────────

/// Context available to every framework resolver.
pub struct ResolutionContext<'a> {
    /// Absolute path to the project root.
    pub project_root: &'a Path,
    /// Languages detected in this project (from A1 framework detection).
    pub project_languages: &'a [Language],
    /// Read-only access to named-symbol caches.
    pub caches: &'a ResolverCaches,
    /// Resolved tsconfig / path-alias map for the project.
    pub path_aliases: &'a PathAliasMap,
    /// All known file paths in the project.
    pub known_files: &'a [String],
}

impl<'a> ResolutionContext<'a> {
    pub fn read_file(&self, relative_path: &str) -> Option<String> {
        let full = self.project_root.join(relative_path);
        std::fs::read_to_string(&full).ok()
    }

    pub fn file_exists(&self, relative_path: &str) -> bool {
        // Check the known-files set first (O(1) on average)
        if self.known_files.iter().any(|f| f == relative_path) {
            return true;
        }
        // Fall back to filesystem
        self.project_root.join(relative_path).exists()
    }

    pub fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        self.caches
            .name_cache
            .get(&name.to_string())
            .unwrap_or_default()
    }

    pub fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        self.caches
            .node_cache
            .get(&file_path.to_string())
            .unwrap_or_default()
    }
}

// ─── Extraction result ────────────────────────────────────────────────────────

/// Result of a framework's extraction pass over a single file.
#[derive(Debug, Default)]
pub struct FrameworkExtractionResult {
    /// New synthetic nodes (e.g. route nodes, DI binding nodes).
    pub nodes: Vec<Node>,
    /// New edges linking framework nodes to handler / implementation nodes.
    pub edges: Vec<Edge>,
    /// New unresolved references emitted for handler names etc.
    pub unresolved_refs: Vec<codewiki_core::UnresolvedRef>,
}

impl FrameworkExtractionResult {
    pub fn empty() -> Self {
        Self::default()
    }
}

// ─── Trait ───────────────────────────────────────────────────────────────────

/// A pluggable, per-framework resolver.
///
/// All methods receive shared references and must be `Send + Sync` so the
/// resolver pool can distribute work across rayon threads.
pub trait FrameworkResolver: Send + Sync {
    /// Short identifier. Examples: "express", "django", "axum", "cargo-workspace".
    fn name(&self) -> &'static str;

    /// Languages this resolver understands. `None` means language-agnostic.
    fn languages(&self) -> Option<&'static [Language]>;

    /// Return `true` if this resolver should be active for the given project.
    /// Called once per project, not per file or per reference.
    fn detect(&self, context: &ResolutionContext<'_>) -> bool;

    /// Attempt to resolve `reference` to a concrete `ResolvedEdge`.
    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &ResolutionContext<'_>,
    ) -> Result<Option<ResolvedEdge>, CodeWikiError>;

    /// Extract framework-specific nodes and edges from a single file's source.
    fn extract(
        &self,
        file_path: &Path,
        content: &str,
        context: &ResolutionContext<'_>,
    ) -> Result<FrameworkExtractionResult, CodeWikiError>;

    /// Minimum confidence below which this resolver's result is discarded.
    fn min_confidence(&self) -> f32 {
        0.5
    }
}

// ─── Helper to build a ResolvedEdge ──────────────────────────────────────────

pub(crate) fn make_resolved_edge(
    reference: &UnresolvedRef,
    target_node_id: String,
    confidence: f32,
    resolver_name: &str,
) -> ResolvedEdge {
    use codewiki_core::{Edge, EdgeKind};
    ResolvedEdge {
        edge: Edge {
            id: format!("{}->{}", reference.from_node_id, target_node_id),
            source_id: reference.from_node_id.clone(),
            target_id: target_node_id,
            kind: EdgeKind::References,
            confidence: Some(confidence),
            provenance: Some(format!("framework-{}", resolver_name)),
            ..Default::default()
        },
        resolved_from: ResolvedFromRef {
            from_node_id: reference.from_node_id.clone(),
            reference_name: reference.reference_name.clone(),
            reference_kind: reference.reference_kind.clone(),
            unresolved_ref_id: reference.id.parse::<i64>().unwrap_or(0),
        },
        confidence,
        resolved_by: ResolvedBy::FrameworkResolver(resolver_name.to_string()),
    }
}

// ─── Registry ────────────────────────────────────────────────────────────────

/// Holds all registered framework resolvers.
///
/// `Clone` is cheap: each entry is `Arc<dyn FrameworkResolver>`, so cloning
/// the registry only copies Arc pointer counts, not the resolver structs.
#[derive(Clone)]
pub struct FrameworkResolverRegistry {
    resolvers: Vec<Arc<dyn FrameworkResolver>>,
}

impl FrameworkResolverRegistry {
    /// Create a registry with all 14 built-in framework resolvers.
    pub fn default_registry() -> Self {
        let resolvers: Vec<Arc<dyn FrameworkResolver>> = vec![
            // Angular — T-Angular
            Arc::new(angular::AngularResolver::new()),
            // Web frameworks — T-307
            Arc::new(express::ExpressResolver),
            Arc::new(django::DjangoResolver),
            Arc::new(flask::FlaskResolver),
            Arc::new(fastapi::FastApiResolver),
            Arc::new(rails::RailsResolver),
            Arc::new(laravel::LaravelResolver),
            Arc::new(nestjs::NestJsResolver),
            Arc::new(spring::SpringResolver),
            // More frameworks — T-308
            Arc::new(go::GinResolver),
            Arc::new(csharp::AspNetResolver),
            Arc::new(swift::VaporResolver),
            Arc::new(react::ReactResolver),
            Arc::new(svelte::SvelteResolver),
            Arc::new(vue::VueResolver),
            Arc::new(cargo_workspace::CargoWorkspaceResolver::new()),
        ];
        Self { resolvers }
    }

    /// Return all resolvers active for the given context.
    pub fn active_resolvers<'a>(
        &'a self,
        context: &ResolutionContext<'_>,
    ) -> Vec<&'a Arc<dyn FrameworkResolver>> {
        self.resolvers
            .iter()
            .filter(|r| {
                // Language filter — skip if project has none of the resolver's languages
                if let Some(langs) = r.languages() {
                    if !context.project_languages.is_empty()
                        && !langs
                            .iter()
                            .any(|l| context.project_languages.contains(l))
                    {
                        return false;
                    }
                }
                // Detect filter
                r.detect(context)
            })
            .collect()
    }

    pub fn all_resolvers(&self) -> &[Arc<dyn FrameworkResolver>] {
        &self.resolvers
    }
}

impl Default for FrameworkResolverRegistry {
    fn default() -> Self {
        Self::default_registry()
    }
}
