/// Graph traversal algorithms — BFS, DFS, callers/callees, impact, type hierarchy,
/// path finding, cycle detection. Port of research-source/src/graph/traversal.ts.
use crate::queries::{edges as eq, nodes as nq};
use codewiki_core::{CodeWikiError, Edge, EdgeKind, Node, Subgraph};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet, VecDeque};

/// A single step in a path: the node and optionally the edge that led to it.
type PathStep = (Node, Option<Edge>);

pub struct GraphTraverser<'a> {
    conn: &'a Connection,
}

#[derive(Debug, Clone)]
pub struct TraversalOptions {
    pub max_depth: usize,
    pub edge_kinds: Vec<EdgeKind>,
    pub direction: TraversalDirection,
    pub limit: usize,
    pub include_start: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TraversalDirection {
    Outgoing,
    Incoming,
    Both,
}

impl Default for TraversalOptions {
    fn default() -> Self {
        Self {
            max_depth: usize::MAX,
            edge_kinds: vec![],
            direction: TraversalDirection::Outgoing,
            limit: 1000,
            include_start: true,
        }
    }
}

impl<'a> GraphTraverser<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    fn get_adjacent_edges(
        &self,
        node_id: &str,
        direction: &TraversalDirection,
        kinds: &[EdgeKind],
    ) -> Result<Vec<Edge>, CodeWikiError> {
        let kinds_opt = if kinds.is_empty() { None } else { Some(kinds) };
        match direction {
            TraversalDirection::Outgoing => eq::get_outgoing_edges(self.conn, node_id, kinds_opt),
            TraversalDirection::Incoming => eq::get_incoming_edges(self.conn, node_id, kinds_opt),
            TraversalDirection::Both => {
                let mut out = eq::get_outgoing_edges(self.conn, node_id, kinds_opt)?;
                out.extend(eq::get_incoming_edges(self.conn, node_id, kinds_opt)?);
                Ok(out)
            }
        }
    }

    /// Breadth-first traversal.
    pub fn traverse_bfs(
        &self,
        start_id: &str,
        opts: &TraversalOptions,
    ) -> Result<Subgraph, CodeWikiError> {
        let start_node = match nq::get_node_by_id(self.conn, start_id)? {
            Some(n) => n,
            None => return Ok(Subgraph::default()),
        };

        let mut nodes: HashMap<String, Node> = HashMap::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();

        struct Step {
            node_id: String,
            edge: Option<Edge>,
            depth: usize,
        }

        let mut queue: VecDeque<Step> = VecDeque::new();
        if opts.include_start {
            nodes.insert(start_node.id.clone(), start_node.clone());
        }
        queue.push_back(Step {
            node_id: start_node.id.clone(),
            edge: None,
            depth: 0,
        });

        while let Some(step) = queue.pop_front() {
            if nodes.len() >= opts.limit {
                break;
            }
            if visited.contains(&step.node_id) {
                continue;
            }
            visited.insert(step.node_id.clone());

            if let Some(edge) = step.edge {
                edges.push(edge);
            }

            if step.depth >= opts.max_depth {
                continue;
            }

            let mut adj =
                self.get_adjacent_edges(&step.node_id, &opts.direction, &opts.edge_kinds)?;
            // Sort: contains=0, calls=1, rest=2
            adj.sort_by_key(|e| {
                let k = serde_json::to_value(&e.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default();
                if k == "contains" {
                    0u8
                } else if k == "calls" {
                    1
                } else {
                    2
                }
            });

            let want_ids: Vec<String> = adj
                .iter()
                .map(|e| {
                    if e.source_id == step.node_id {
                        e.target_id.clone()
                    } else {
                        e.source_id.clone()
                    }
                })
                .filter(|id| !visited.contains(id))
                .collect();
            let neighbor_map: HashMap<String, Node> = nq::get_nodes_by_ids(self.conn, &want_ids)?
                .into_iter()
                .map(|n| (n.id.clone(), n))
                .collect();

            for adj_edge in adj {
                let next_id = if adj_edge.source_id == step.node_id {
                    adj_edge.target_id.clone()
                } else {
                    adj_edge.source_id.clone()
                };
                if visited.contains(&next_id) {
                    continue;
                }
                if let Some(next_node) = neighbor_map.get(&next_id) {
                    nodes.insert(next_node.id.clone(), next_node.clone());
                    queue.push_back(Step {
                        node_id: next_id,
                        edge: Some(adj_edge),
                        depth: step.depth + 1,
                    });
                }
            }
        }

        Ok(Subgraph {
            nodes,
            edges,
            roots: vec![start_id.to_string()],
        })
    }

    /// Depth-first traversal.
    pub fn traverse_dfs(
        &self,
        start_id: &str,
        opts: &TraversalOptions,
    ) -> Result<Subgraph, CodeWikiError> {
        let start_node = match nq::get_node_by_id(self.conn, start_id)? {
            Some(n) => n,
            None => return Ok(Subgraph::default()),
        };

        let mut nodes: HashMap<String, Node> = HashMap::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();

        if opts.include_start {
            nodes.insert(start_node.id.clone(), start_node.clone());
        }

        self.dfs_recursive(
            &start_node.id,
            0,
            opts,
            &mut nodes,
            &mut edges,
            &mut visited,
        )?;

        Ok(Subgraph {
            nodes,
            edges,
            roots: vec![start_id.to_string()],
        })
    }

    fn dfs_recursive(
        &self,
        node_id: &str,
        depth: usize,
        opts: &TraversalOptions,
        nodes: &mut HashMap<String, Node>,
        edges: &mut Vec<Edge>,
        visited: &mut HashSet<String>,
    ) -> Result<(), CodeWikiError> {
        if visited.contains(node_id) || nodes.len() >= opts.limit || depth >= opts.max_depth {
            return Ok(());
        }
        visited.insert(node_id.to_string());

        let adj = self.get_adjacent_edges(node_id, &opts.direction, &opts.edge_kinds)?;
        let want_ids: Vec<String> = adj
            .iter()
            .map(|e| {
                if e.source_id == node_id {
                    e.target_id.clone()
                } else {
                    e.source_id.clone()
                }
            })
            .filter(|id| !visited.contains(id))
            .collect();
        let neighbor_map: HashMap<String, Node> = nq::get_nodes_by_ids(self.conn, &want_ids)?
            .into_iter()
            .map(|n| (n.id.clone(), n))
            .collect();

        for edge in adj {
            let next_id = if edge.source_id == node_id {
                edge.target_id.clone()
            } else {
                edge.source_id.clone()
            };
            if visited.contains(&next_id) {
                continue;
            }
            if let Some(next_node) = neighbor_map.get(&next_id) {
                nodes.insert(next_node.id.clone(), next_node.clone());
                edges.push(edge);
                let next_id_clone = next_id.clone();
                self.dfs_recursive(&next_id_clone, depth + 1, opts, nodes, edges, visited)?;
            }
        }
        Ok(())
    }

    /// Get all callers of a node up to `max_depth` hops.
    pub fn get_callers(
        &self,
        node_id: &str,
        max_depth: usize,
    ) -> Result<Vec<(Node, Edge)>, CodeWikiError> {
        let mut result = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        self.get_callers_recursive(node_id, max_depth, 0, &mut result, &mut visited)?;
        Ok(result)
    }

    fn get_callers_recursive(
        &self,
        node_id: &str,
        max_depth: usize,
        depth: usize,
        result: &mut Vec<(Node, Edge)>,
        visited: &mut HashSet<String>,
    ) -> Result<(), CodeWikiError> {
        if depth >= max_depth || visited.contains(node_id) {
            return Ok(());
        }
        visited.insert(node_id.to_string());

        let incoming = eq::get_incoming_edges(
            self.conn,
            node_id,
            Some(&[EdgeKind::Calls, EdgeKind::References, EdgeKind::Imports]),
        )?;
        if incoming.is_empty() {
            return Ok(());
        }

        let source_ids: Vec<String> = incoming.iter().map(|e| e.source_id.clone()).collect();
        let caller_map: HashMap<String, Node> = nq::get_nodes_by_ids(self.conn, &source_ids)?
            .into_iter()
            .map(|n| (n.id.clone(), n))
            .collect();

        for edge in incoming {
            if let Some(caller) = caller_map.get(&edge.source_id) {
                if !visited.contains(&caller.id) {
                    result.push((caller.clone(), edge));
                    self.get_callers_recursive(
                        &caller.id.clone(),
                        max_depth,
                        depth + 1,
                        result,
                        visited,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Get all callees of a node up to `max_depth` hops.
    pub fn get_callees(
        &self,
        node_id: &str,
        max_depth: usize,
    ) -> Result<Vec<(Node, Edge)>, CodeWikiError> {
        let mut result = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        self.get_callees_recursive(node_id, max_depth, 0, &mut result, &mut visited)?;
        Ok(result)
    }

    fn get_callees_recursive(
        &self,
        node_id: &str,
        max_depth: usize,
        depth: usize,
        result: &mut Vec<(Node, Edge)>,
        visited: &mut HashSet<String>,
    ) -> Result<(), CodeWikiError> {
        if depth >= max_depth || visited.contains(node_id) {
            return Ok(());
        }
        visited.insert(node_id.to_string());

        let outgoing = eq::get_outgoing_edges(
            self.conn,
            node_id,
            Some(&[EdgeKind::Calls, EdgeKind::References, EdgeKind::Imports]),
        )?;
        if outgoing.is_empty() {
            return Ok(());
        }

        let target_ids: Vec<String> = outgoing.iter().map(|e| e.target_id.clone()).collect();
        let callee_map: HashMap<String, Node> = nq::get_nodes_by_ids(self.conn, &target_ids)?
            .into_iter()
            .map(|n| (n.id.clone(), n))
            .collect();

        for edge in outgoing {
            if let Some(callee) = callee_map.get(&edge.target_id) {
                if !visited.contains(&callee.id) {
                    result.push((callee.clone(), edge));
                    self.get_callees_recursive(
                        &callee.id.clone(),
                        max_depth,
                        depth + 1,
                        result,
                        visited,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Impact radius: reverse-reach subgraph.
    pub fn get_impact_radius(
        &self,
        node_id: &str,
        max_depth: usize,
    ) -> Result<Subgraph, CodeWikiError> {
        let focal = match nq::get_node_by_id(self.conn, node_id)? {
            Some(n) => n,
            None => return Ok(Subgraph::default()),
        };

        let mut nodes: HashMap<String, Node> = HashMap::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();

        nodes.insert(focal.id.clone(), focal);
        self.impact_recursive(node_id, max_depth, 0, &mut nodes, &mut edges, &mut visited)?;

        Ok(Subgraph {
            nodes,
            edges,
            roots: vec![node_id.to_string()],
        })
    }

    fn impact_recursive(
        &self,
        node_id: &str,
        max_depth: usize,
        depth: usize,
        nodes: &mut HashMap<String, Node>,
        edges: &mut Vec<Edge>,
        visited: &mut HashSet<String>,
    ) -> Result<(), CodeWikiError> {
        if depth >= max_depth || visited.contains(node_id) {
            return Ok(());
        }
        visited.insert(node_id.to_string());

        // For container nodes, recurse into children at same depth
        if let Some(focal) = nq::get_node_by_id(self.conn, node_id)? {
            let kind_str = serde_json::to_value(&focal.kind)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            let container_kinds = [
                "class",
                "interface",
                "struct",
                "trait",
                "protocol",
                "module",
                "enum",
            ];
            if container_kinds.contains(&kind_str.as_str()) {
                let contains_edges =
                    eq::get_outgoing_edges(self.conn, node_id, Some(&[EdgeKind::Contains]))?;
                if !contains_edges.is_empty() {
                    let child_ids: Vec<String> =
                        contains_edges.iter().map(|e| e.target_id.clone()).collect();
                    let child_map: HashMap<String, Node> =
                        nq::get_nodes_by_ids(self.conn, &child_ids)?
                            .into_iter()
                            .map(|n| (n.id.clone(), n))
                            .collect();
                    for edge in contains_edges {
                        if let Some(child) = child_map.get(&edge.target_id) {
                            if !visited.contains(&child.id) {
                                nodes.insert(child.id.clone(), child.clone());
                                edges.push(edge);
                                self.impact_recursive(
                                    &child.id.clone(),
                                    max_depth,
                                    depth,
                                    nodes,
                                    edges,
                                    visited,
                                )?;
                            }
                        }
                    }
                }
            }
        }

        let incoming = eq::get_incoming_edges(self.conn, node_id, None)?;
        if incoming.is_empty() {
            return Ok(());
        }
        let src_ids: Vec<String> = incoming.iter().map(|e| e.source_id.clone()).collect();
        let src_map: HashMap<String, Node> = nq::get_nodes_by_ids(self.conn, &src_ids)?
            .into_iter()
            .map(|n| (n.id.clone(), n))
            .collect();

        for edge in incoming {
            if let Some(src) = src_map.get(&edge.source_id) {
                if !nodes.contains_key(&src.id) {
                    nodes.insert(src.id.clone(), src.clone());
                    edges.push(edge);
                    self.impact_recursive(
                        &src.id.clone(),
                        max_depth,
                        depth + 1,
                        nodes,
                        edges,
                        visited,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Type hierarchy: ancestors + descendants via extends/implements.
    pub fn get_type_hierarchy(&self, node_id: &str) -> Result<Subgraph, CodeWikiError> {
        let focal = match nq::get_node_by_id(self.conn, node_id)? {
            Some(n) => n,
            None => return Ok(Subgraph::default()),
        };

        let mut nodes: HashMap<String, Node> = HashMap::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();

        nodes.insert(focal.id.clone(), focal);
        self.type_ancestors(node_id, &mut nodes, &mut edges, &mut visited)?;
        visited.clear();
        self.type_descendants(node_id, &mut nodes, &mut edges, &mut visited)?;

        Ok(Subgraph {
            nodes,
            edges,
            roots: vec![node_id.to_string()],
        })
    }

    fn type_ancestors(
        &self,
        node_id: &str,
        nodes: &mut HashMap<String, Node>,
        edges: &mut Vec<Edge>,
        visited: &mut HashSet<String>,
    ) -> Result<(), CodeWikiError> {
        if visited.contains(node_id) {
            return Ok(());
        }
        visited.insert(node_id.to_string());
        let outgoing = eq::get_outgoing_edges(
            self.conn,
            node_id,
            Some(&[EdgeKind::Extends, EdgeKind::Implements]),
        )?;
        if outgoing.is_empty() {
            return Ok(());
        }
        let parent_ids: Vec<String> = outgoing.iter().map(|e| e.target_id.clone()).collect();
        let parent_map: HashMap<String, Node> = nq::get_nodes_by_ids(self.conn, &parent_ids)?
            .into_iter()
            .map(|n| (n.id.clone(), n))
            .collect();
        for edge in outgoing {
            if let Some(parent) = parent_map.get(&edge.target_id) {
                if !nodes.contains_key(&parent.id) {
                    nodes.insert(parent.id.clone(), parent.clone());
                    edges.push(edge);
                    self.type_ancestors(&parent.id.clone(), nodes, edges, visited)?;
                }
            }
        }
        Ok(())
    }

    fn type_descendants(
        &self,
        node_id: &str,
        nodes: &mut HashMap<String, Node>,
        edges: &mut Vec<Edge>,
        visited: &mut HashSet<String>,
    ) -> Result<(), CodeWikiError> {
        if visited.contains(node_id) {
            return Ok(());
        }
        visited.insert(node_id.to_string());
        let incoming = eq::get_incoming_edges(
            self.conn,
            node_id,
            Some(&[EdgeKind::Extends, EdgeKind::Implements]),
        )?;
        if incoming.is_empty() {
            return Ok(());
        }
        let child_ids: Vec<String> = incoming.iter().map(|e| e.source_id.clone()).collect();
        let child_map: HashMap<String, Node> = nq::get_nodes_by_ids(self.conn, &child_ids)?
            .into_iter()
            .map(|n| (n.id.clone(), n))
            .collect();
        for edge in incoming {
            if let Some(child) = child_map.get(&edge.source_id) {
                if !nodes.contains_key(&child.id) {
                    nodes.insert(child.id.clone(), child.clone());
                    edges.push(edge);
                    self.type_descendants(&child.id.clone(), nodes, edges, visited)?;
                }
            }
        }
        Ok(())
    }

    /// Find shortest path between two nodes (BFS).
    pub fn find_path(
        &self,
        from_id: &str,
        to_id: &str,
        edge_kinds: &[EdgeKind],
    ) -> Result<Option<Vec<PathStep>>, CodeWikiError> {
        let from_node = match nq::get_node_by_id(self.conn, from_id)? {
            Some(n) => n,
            None => return Ok(None),
        };
        let to_node = nq::get_node_by_id(self.conn, to_id)?;
        if to_node.is_none() {
            return Ok(None);
        }

        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, Vec<PathStep>)> = VecDeque::new();
        queue.push_back((from_id.to_string(), vec![(from_node, None)]));

        while let Some((current_id, path)) = queue.pop_front() {
            if current_id == to_id {
                return Ok(Some(path));
            }
            if visited.contains(&current_id) {
                continue;
            }
            visited.insert(current_id.clone());

            let kinds_opt = if edge_kinds.is_empty() {
                None
            } else {
                Some(edge_kinds)
            };
            let outgoing = eq::get_outgoing_edges(self.conn, &current_id, kinds_opt)?;
            let want_ids: Vec<String> = outgoing
                .iter()
                .map(|e| e.target_id.clone())
                .filter(|id| !visited.contains(id))
                .collect();
            let next_map: HashMap<String, Node> = nq::get_nodes_by_ids(self.conn, &want_ids)?
                .into_iter()
                .map(|n| (n.id.clone(), n))
                .collect();

            for edge in outgoing {
                if !visited.contains(&edge.target_id) {
                    if let Some(next_node) = next_map.get(&edge.target_id) {
                        let mut new_path = path.clone();
                        new_path.push((next_node.clone(), Some(edge.clone())));
                        queue.push_back((edge.target_id.clone(), new_path));
                    }
                }
            }
        }
        Ok(None)
    }

    /// All direct incoming edges (one-hop only).
    pub fn find_usages(&self, node_id: &str) -> Result<Vec<(Node, Edge)>, CodeWikiError> {
        let incoming = eq::get_incoming_edges(self.conn, node_id, None)?;
        if incoming.is_empty() {
            return Ok(vec![]);
        }
        let src_ids: Vec<String> = incoming.iter().map(|e| e.source_id.clone()).collect();
        let src_map: HashMap<String, Node> = nq::get_nodes_by_ids(self.conn, &src_ids)?
            .into_iter()
            .map(|n| (n.id.clone(), n))
            .collect();
        let mut result = Vec::new();
        for edge in incoming {
            if let Some(src) = src_map.get(&edge.source_id) {
                result.push((src.clone(), edge));
            }
        }
        Ok(result)
    }

    /// Get containment ancestors (immediate parent chain).
    pub fn get_ancestors(&self, node_id: &str) -> Result<Vec<Node>, CodeWikiError> {
        let mut ancestors = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut current_id = node_id.to_string();

        loop {
            if visited.contains(&current_id) {
                break;
            }
            visited.insert(current_id.clone());

            let containing =
                eq::get_incoming_edges(self.conn, &current_id, Some(&[EdgeKind::Contains]))?;
            match containing.into_iter().next() {
                Some(edge) => {
                    if let Some(parent) = nq::get_node_by_id(self.conn, &edge.source_id)? {
                        ancestors.push(parent.clone());
                        current_id = parent.id;
                    } else {
                        break;
                    }
                }
                None => break,
            }
        }
        Ok(ancestors)
    }

    /// Get immediate children (contains edges).
    pub fn get_children(&self, node_id: &str) -> Result<Vec<Node>, CodeWikiError> {
        let contains_edges =
            eq::get_outgoing_edges(self.conn, node_id, Some(&[EdgeKind::Contains]))?;
        if contains_edges.is_empty() {
            return Ok(vec![]);
        }
        let child_ids: Vec<String> = contains_edges.iter().map(|e| e.target_id.clone()).collect();
        let child_map: HashMap<String, Node> = nq::get_nodes_by_ids(self.conn, &child_ids)?
            .into_iter()
            .map(|n| (n.id.clone(), n))
            .collect();
        let mut children = Vec::new();
        for edge in contains_edges {
            if let Some(child) = child_map.get(&edge.target_id) {
                children.push(child.clone());
            }
        }
        Ok(children)
    }

    /// Detect circular dependencies (file-level DFS with recursion stack).
    pub fn find_circular_dependencies(
        &self,
        conn: &Connection,
    ) -> Result<Vec<Vec<String>>, CodeWikiError> {
        use crate::queries::files::get_all_files;
        let files = get_all_files(conn)?;
        let mut cycles: Vec<Vec<String>> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut recursion_stack: HashSet<String> = HashSet::new();

        for file in &files {
            let path = file.path.to_string_lossy().to_string();
            if !visited.contains(&path) {
                self.cycle_dfs(
                    conn,
                    &path,
                    &mut visited,
                    &mut recursion_stack,
                    &mut vec![],
                    &mut cycles,
                )?;
            }
        }
        Ok(cycles)
    }

    fn cycle_dfs(
        &self,
        conn: &Connection,
        file_path: &str,
        visited: &mut HashSet<String>,
        recursion_stack: &mut HashSet<String>,
        path: &mut Vec<String>,
        cycles: &mut Vec<Vec<String>>,
    ) -> Result<(), CodeWikiError> {
        use crate::queries::edges::get_outgoing_edges;
        use crate::queries::nodes::get_nodes_by_file;

        if recursion_stack.contains(file_path) {
            let cycle_start = path.iter().position(|p| p == file_path);
            if let Some(start) = cycle_start {
                cycles.push(path[start..].to_vec());
            }
            return Ok(());
        }
        if visited.contains(file_path) {
            return Ok(());
        }

        visited.insert(file_path.to_string());
        recursion_stack.insert(file_path.to_string());
        path.push(file_path.to_string());

        // Find file dependencies via import edges
        let nodes = get_nodes_by_file(conn, file_path)?;
        let file_node = nodes.iter().find(|n| {
            serde_json::to_value(&n.kind)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default()
                == "file"
        });

        if let Some(fnode) = file_node {
            let import_edges = get_outgoing_edges(conn, &fnode.id, Some(&[EdgeKind::Imports]))?;
            for edge in import_edges {
                if let Some(target) = nq::get_node_by_id(conn, &edge.target_id)? {
                    if target.file_path != file_path {
                        self.cycle_dfs(
                            conn,
                            &target.file_path,
                            visited,
                            recursion_stack,
                            path,
                            cycles,
                        )?;
                    }
                }
            }
        }

        recursion_stack.remove(file_path);
        path.pop();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open_in_memory;
    use crate::queries::edges::insert_edge;
    use crate::queries::nodes::insert_node;
    use codewiki_core::{Edge, EdgeKind, Language, Node, NodeKind};

    fn make_node(id: &str) -> Node {
        Node {
            id: id.to_string(),
            name: id.to_string(),
            qualified_name: id.to_string(),
            kind: NodeKind::Function,
            language: Language::TypeScript,
            file_path: "src/x.ts".to_string(),
            ..Default::default()
        }
    }

    fn make_edge(from: &str, to: &str, kind: EdgeKind) -> Edge {
        Edge {
            id: format!("{}->{}", from, to),
            source_id: from.to_string(),
            target_id: to.to_string(),
            kind,
            ..Default::default()
        }
    }

    #[test]
    fn bfs_finds_nodes() {
        let conn = open_in_memory().unwrap();
        // A -> B -> C
        for id in &["A", "B", "C"] {
            insert_node(&conn, &make_node(id)).unwrap();
        }
        insert_edge(&conn, &make_edge("A", "B", EdgeKind::Calls)).unwrap();
        insert_edge(&conn, &make_edge("B", "C", EdgeKind::Calls)).unwrap();

        let traverser = GraphTraverser::new(&conn);
        let subgraph = traverser
            .traverse_bfs("A", &TraversalOptions::default())
            .unwrap();
        assert_eq!(subgraph.nodes.len(), 3);
    }

    #[test]
    fn callers_finds_callers() {
        let conn = open_in_memory().unwrap();
        for id in &["main", "helper", "util"] {
            insert_node(&conn, &make_node(id)).unwrap();
        }
        insert_edge(&conn, &make_edge("main", "helper", EdgeKind::Calls)).unwrap();
        insert_edge(&conn, &make_edge("util", "helper", EdgeKind::Calls)).unwrap();

        let traverser = GraphTraverser::new(&conn);
        let callers = traverser.get_callers("helper", 1).unwrap();
        assert_eq!(callers.len(), 2);
    }

    #[test]
    fn cycle_detection() {
        let conn = open_in_memory().unwrap();
        // A -> B -> A (cycle via Calls for this test we just verify no panic)
        for id in &["A", "B"] {
            insert_node(&conn, &make_node(id)).unwrap();
        }
        insert_edge(&conn, &make_edge("A", "B", EdgeKind::Calls)).unwrap();
        insert_edge(&conn, &make_edge("B", "A", EdgeKind::Calls)).unwrap();

        let traverser = GraphTraverser::new(&conn);
        // BFS with limit should terminate even with cycles
        let subgraph = traverser
            .traverse_bfs(
                "A",
                &TraversalOptions {
                    limit: 100,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(!subgraph.nodes.is_empty());
    }
}
