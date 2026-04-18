//! JSON tree data structure with collapse/expand state and search filtering.
//!
//! The tree is rebuilt from a `serde_json::Value` each time the raw text
//! parses successfully. Collapse state is preserved across rebuilds by
//! matching nodes on their *path* (a string like `"foo.bar[2].baz"`).

use std::collections::HashSet;

use serde_json::Value;

// ── node kind ────────────────────────────────────────────────────────────────

/// The kind of a JSON value, used for display and editing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    Object { child_count: usize },
    Array { child_count: usize },
    String,
    Number,
    Bool,
    Null,
}

impl NodeKind {
    pub fn is_container(&self) -> bool {
        matches!(self, NodeKind::Object { .. } | NodeKind::Array { .. })
    }

    /// Short type label shown in the tree.
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            NodeKind::Object { .. } => "object",
            NodeKind::Array { .. } => "array",
            NodeKind::String => "string",
            NodeKind::Number => "number",
            NodeKind::Bool => "bool",
            NodeKind::Null => "null",
        }
    }
}

// ── tree node ─────────────────────────────────────────────────────────────────

/// A single flattened node in the tree display list.
///
/// The tree is stored as a flat `Vec<FlatNode>` that is re-generated
/// whenever the collapse state or the underlying JSON changes.
#[derive(Debug, Clone)]
pub struct FlatNode {
    /// Display depth (root = 0).
    pub depth: usize,
    /// Key string for object children; array index as string for array
    /// children; empty for root.
    pub key: String,
    /// The JSON value at this node.
    pub value: Value,
    /// Pre-computed kind.
    pub kind: NodeKind,
    /// Dot-notation path from root, e.g. `"users[0].name"`.
    pub path: String,
    /// Whether this node is currently collapsed (only meaningful for
    /// containers).
    pub collapsed: bool,
    /// Whether this node matches the current search query.
    pub search_match: bool,
    /// Line number in the raw text (0-based) corresponding to this node.
    /// Populated by `JsonTree::sync_line_numbers` after every successful parse.
    /// `None` until that first sync or for formats whose `line_for_path` is
    /// not yet implemented.
    pub line_number: Option<usize>,
}

impl FlatNode {
    /// Short inline value string shown next to the key for leaf nodes.
    ///
    /// For containers the child count comes from `NodeKind` — `value` is
    /// stored only for scalars, so no deep-clone of the subtree is needed.
    pub fn value_display(&self) -> String {
        match &self.kind {
            NodeKind::Object { child_count } => format!("{{{}}}", child_count),
            NodeKind::Array { child_count } => format!("[{}]", child_count),
            _ => match &self.value {
                Value::String(s) => format!("\"{}\"", truncate(s, 60)),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => "null".to_string(),
                _ => String::new(), // unreachable for scalars
            },
        }
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

// ── json tree ─────────────────────────────────────────────────────────────────

/// The top-level tree model. Wraps the source `Value` and the collapse set,
/// and provides the flat-list view that the UI renders.
pub struct JsonTree {
    /// The parsed JSON value.
    pub root: Value,
    /// Paths of nodes that are currently collapsed.
    collapsed: HashSet<String>,
    /// Pre-built flat display list. Rebuilt on each structure / collapse change.
    flat: Vec<FlatNode>,
    /// Current search / filter string (empty = no filter).
    search_query: String,
}

impl JsonTree {
    /// Construct a tree from a `Value`, importing any existing collapse state
    /// from `previous_collapsed`.
    pub fn from_value(value: Value, previous_collapsed: HashSet<String>) -> Self {
        let mut tree = Self {
            root: value,
            collapsed: previous_collapsed,
            flat: Vec::new(),
            search_query: String::new(),
        };
        tree.rebuild_flat();
        tree
    }

    /// Return the set of collapsed paths so it can be handed to the next
    /// rebuild after a text change.
    pub fn take_collapsed(&self) -> HashSet<String> {
        self.collapsed.clone()
    }

    /// Number of visible rows (after filtering and respecting collapse).
    pub fn visible_node_count(&self) -> usize {
        self.flat.len()
    }

    /// Immutable slice of the flat node list.
    pub fn flat_nodes(&self) -> &[FlatNode] {
        &self.flat
    }

    /// Toggle collapse/expand of the node at flat index `idx`.
    /// Returns `true` if the node is a container (operation was meaningful).
    pub fn toggle_collapse(&mut self, idx: usize) -> bool {
        let Some(node) = self.flat.get(idx) else {
            return false;
        };
        if !node.kind.is_container() {
            return false;
        }
        let path = node.path.clone();
        if self.collapsed.contains(&path) {
            self.collapsed.remove(&path);
        } else {
            self.collapsed.insert(path);
        }
        self.rebuild_flat();
        true
    }

    /// Set the search query and rebuild.
    pub fn set_search(&mut self, query: &str) {
        self.search_query = query.to_string();
        self.rebuild_flat();
    }

    /// Clear search.
    pub fn clear_search(&mut self) {
        self.search_query.clear();
        self.rebuild_flat();
    }

    /// Ensure the node at `path` and all its ancestors are expanded so it
    /// will appear in the flat display list.
    ///
    /// This is used after a search to make the selected result visible once
    /// the filter is cleared and the full tree is restored.
    pub fn expand_to_path(&mut self, path: &str) {
        // Remove `path` itself from the collapsed set (it might be a container
        // the user wants to see open).
        self.collapsed.remove(path);

        // Walk up the ancestry chain and remove each ancestor from collapsed.
        let mut current = path.to_string();
        loop {
            match split_path(&current) {
                Ok((parent, _)) if !parent.is_empty() => {
                    self.collapsed.remove(parent);
                    current = parent.to_string();
                }
                _ => break,
            }
        }
        self.rebuild_flat();
    }

    /// Find the flat-list index for the node whose path equals `path`.
    /// Returns `None` if the node is not currently visible (e.g. inside a
    /// collapsed section — call `expand_to_path` first).
    pub fn index_of_path(&self, path: &str) -> Option<usize> {
        self.flat.iter().position(|n| n.path == path)
    }

    /// Populate `FlatNode::line_number` for every visible node by calling
    /// `resolve` with each node's dot-notation path.
    ///
    /// Call this after every successful parse.  The typical closure is:
    /// ```ignore
    /// tree.sync_line_numbers(|path| find_line_for_path(raw, path, format));
    /// ```
    pub fn sync_line_numbers<F>(&mut self, mut resolve: F)
    where
        F: FnMut(&str) -> Option<usize>,
    {
        for node in &mut self.flat {
            node.line_number = resolve(&node.path);
        }
    }

    /// Return the flat-list index of the node that "owns" the given cursor
    /// line — i.e. the node whose `line_number` is the largest value still
    /// ≤ `cursor_line`.
    ///
    /// This gives the most-specific node that *starts at or before* the cursor,
    /// which is the correct semantic for "what am I looking at on this line?"
    ///
    /// Returns `None` when no `line_number` values have been populated yet.
    pub fn find_node_at_line(&self, cursor_line: usize) -> Option<usize> {
        let mut best_idx: Option<usize> = None;
        let mut best_line: usize = 0;
        for (i, node) in self.flat.iter().enumerate() {
            if let Some(line) = node.line_number {
                if line <= cursor_line && (best_idx.is_none() || line >= best_line) {
                    best_line = line;
                    best_idx = Some(i);
                }
            }
        }
        best_idx
    }

    /// Replace the root `Value` in-place, preserving the collapsed set.
    ///
    /// Cheaper than `from_value` because it avoids cloning (and then
    /// discarding) the existing `collapsed` `HashSet`.  Used by the
    /// debounced-reparse path in the event loop.
    pub fn update_value(&mut self, new_value: Value) {
        self.root = new_value;
        self.rebuild_flat();
    }

    /// Update a leaf node by path, replacing its value.
    /// Returns `Ok(())` on success.
    pub fn set_value_at_path(&mut self, path: &str, new_value: Value) -> Result<(), String> {
        set_value(&mut self.root, path, new_value)?;
        self.rebuild_flat();
        Ok(())
    }

    /// Like `set_value_at_path` but **skips** `rebuild_flat`.
    ///
    /// Use this when applying multiple mutations in a batch; call
    /// `rebuild()` once after all mutations are done.
    pub fn set_value_at_path_no_rebuild(&mut self, path: &str, new_value: Value) -> Result<(), String> {
        set_value(&mut self.root, path, new_value)
    }

    /// Explicitly rebuild the flat display list.  Call after a batch of
    /// `set_value_at_path_no_rebuild` mutations.
    pub fn rebuild(&mut self) {
        self.rebuild_flat();
    }

    /// Delete the node at `path`. Returns `Ok(())` on success.
    pub fn delete_at_path(&mut self, path: &str) -> Result<(), String> {
        delete_value(&mut self.root, path)?;
        self.collapsed.remove(path);
        self.rebuild_flat();
        Ok(())
    }

    /// Add a key/value pair to the object at `parent_path`.
    pub fn add_key(
        &mut self,
        parent_path: &str,
        key: String,
        value: Value,
    ) -> Result<(), String> {
        let parent = get_value_mut(&mut self.root, parent_path)?;
        match parent {
            Value::Object(map) => {
                map.insert(key, value);
                self.rebuild_flat();
                Ok(())
            }
            Value::Array(arr) => {
                arr.push(value);
                self.rebuild_flat();
                Ok(())
            }
            _ => Err(format!("Node at '{}' is not an object or array", parent_path)),
        }
    }

    /// Rename the key of the node at `path` to `new_key`.
    pub fn rename_key_at_path(&mut self, path: &str, new_key: String) -> Result<(), String> {
        // We need to find the parent path and old key.
        let (parent_path, old_key) = split_path(path)?;
        let parent = get_value_mut(&mut self.root, parent_path)?;
        match parent {
            Value::Object(map) => {
                if let Some(v) = map.remove(old_key) {
                    map.insert(new_key, v);
                    self.rebuild_flat();
                    Ok(())
                } else {
                    Err(format!("Key '{}' not found", old_key))
                }
            }
            _ => Err(format!("Parent at '{}' is not an object", parent_path)),
        }
    }

    // ── private helpers ───────────────────────────────────────────────────────

    /// Rebuild the flat display list from scratch.
    fn rebuild_flat(&mut self) {
        let mut flat = Vec::new();
        let query = self.search_query.to_lowercase();

        match &self.root {
            Value::Object(_) | Value::Array(_) => {
                flatten_value(
                    &self.root,
                    "",
                    "",
                    0,
                    &self.collapsed,
                    &mut flat,
                );
            }
            _ => {
                // Scalar root (unusual but valid JSON).
                let kind = value_kind(&self.root);
                flat.push(FlatNode {
                    depth: 0,
                    key: String::new(),
                    value: self.root.clone(),
                    kind,
                    path: String::new(),
                    collapsed: false,
                    search_match: false,
                    line_number: None,
                });
            }
        }

        // Apply search filter: mark matching nodes and ensure their ancestors
        // are visible.
        //
        // Two passes instead of three:
        //   Pass 1 — find matches and build the ancestor-inclusion set.
        //   Pass 2 — filter the flat list *and* set search_match in one loop,
        //            avoiding a second call to value_display() per node.
        if !query.is_empty() {
            let mut matching_paths: HashSet<String> = HashSet::new();

            // Pass 1: identify matching nodes and their ancestors.
            for node in &flat {
                let key_lc = node.key.to_lowercase();
                let val_lc = node.value_display().to_lowercase();
                if key_lc.contains(&query) || val_lc.contains(&query) {
                    matching_paths.insert(node.path.clone());
                    let mut p = node.path.as_str();
                    loop {
                        let Ok((parent, _)) = split_path(p) else { break };
                        if parent.is_empty() {
                            break;
                        }
                        matching_paths.insert(parent.to_string());
                        p = parent;
                    }
                }
            }

            // Pass 2: filter and flag in one sweep (no second value_display call).
            let mut filtered = Vec::with_capacity(flat.len());
            for mut node in flat {
                if matching_paths.contains(&node.path) || node.path.is_empty() {
                    let key_match = node.key.to_lowercase().contains(&query);
                    let val_match = node.value_display().to_lowercase().contains(&query);
                    node.search_match = key_match || val_match;
                    filtered.push(node);
                }
            }
            flat = filtered;
        }

        self.flat = flat;
    }

    /// Find the first flat-node index whose key or value matches the query.
    pub fn first_search_match(&self) -> Option<usize> {
        self.flat.iter().position(|n| n.search_match)
    }
}

// ── free functions: tree traversal ────────────────────────────────────────────

/// Recursively flatten a `Value` into `out`.
fn flatten_value(
    value: &Value,
    key: &str,
    parent_path: &str,
    depth: usize,
    collapsed: &HashSet<String>,
    out: &mut Vec<FlatNode>,
) {
    let path = build_path(parent_path, key, value);
    let kind = value_kind(value);
    let is_collapsed = collapsed.contains(&path);

    // For containers, the child count is already captured in `NodeKind`, so
    // there is no need to deep-clone the entire subtree into `FlatNode.value`.
    // Storing `Value::Null` as a placeholder avoids O(N·D) memory usage.
    let stored_value = match value {
        Value::Object(_) | Value::Array(_) => Value::Null,
        _ => value.clone(),
    };

    out.push(FlatNode {
        depth,
        key: key.to_string(),
        value: stored_value,
        kind: kind.clone(),
        path: path.clone(),
        collapsed: is_collapsed,
        search_match: false,
        line_number: None,
    });

    if is_collapsed {
        return;
    }

    match value {
        Value::Object(map) => {
            for (k, v) in map {
                flatten_value(v, k, &path, depth + 1, collapsed, out);
            }
        }
        Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                flatten_value(v, &i.to_string(), &path, depth + 1, collapsed, out);
            }
        }
        _ => {}
    }
}

fn build_path(parent: &str, key: &str, _value: &Value) -> String {
    if parent.is_empty() && key.is_empty() {
        String::new()
    } else if parent.is_empty() {
        key.to_string()
    } else if key.parse::<usize>().is_ok() {
        format!("{}[{}]", parent, key)
    } else {
        format!("{}.{}", parent, key)
    }
}

fn value_kind(value: &Value) -> NodeKind {
    match value {
        Value::Object(m) => NodeKind::Object { child_count: m.len() },
        Value::Array(a) => NodeKind::Array { child_count: a.len() },
        Value::String(_) => NodeKind::String,
        Value::Number(_) => NodeKind::Number,
        Value::Bool(_) => NodeKind::Bool,
        Value::Null => NodeKind::Null,
    }
}

/// Split a dot-notation path into (parent_path, last_key).
/// e.g. `"foo.bar[2].baz"` → `("foo.bar[2]", "baz")`
///      `"foo"` → `("", "foo")`
pub fn split_path(path: &str) -> Result<(&str, &str), String> {
    if path.is_empty() {
        return Err("Cannot split empty path".to_string());
    }
    // Try array index first: ends with `[N]`
    if path.ends_with(']') {
        if let Some(bracket) = path.rfind('[') {
            let parent = &path[..bracket];
            let key = &path[bracket + 1..path.len() - 1];
            return Ok((parent, key));
        }
    }
    // Otherwise split on the last dot.
    if let Some(dot) = path.rfind('.') {
        Ok((&path[..dot], &path[dot + 1..]))
    } else {
        Ok(("", path))
    }
}

// ── free functions: mutable access by path ────────────────────────────────────

/// Navigate to the node at `path` and return a mutable reference.
fn get_value_mut<'a>(root: &'a mut Value, path: &str) -> Result<&'a mut Value, String> {
    if path.is_empty() {
        return Ok(root);
    }
    let parts = parse_path_parts(path)?;
    let mut cur = root;
    for part in parts {
        cur = match cur {
            Value::Object(map) => map
                .get_mut(&part)
                .ok_or_else(|| format!("Key '{}' not found", part))?,
            Value::Array(arr) => {
                let idx: usize = part
                    .parse()
                    .map_err(|_| format!("'{}' is not a valid index", part))?;
                arr.get_mut(idx)
                    .ok_or_else(|| format!("Index {} out of range", idx))?
            }
            _ => return Err(format!("Cannot index into scalar at '{}'", part)),
        };
    }
    Ok(cur)
}

/// Set the value at `path` to `new_value`.
fn set_value(root: &mut Value, path: &str, new_value: Value) -> Result<(), String> {
    if path.is_empty() {
        *root = new_value;
        return Ok(());
    }
    let (parent_path, last_key) = split_path(path)?;
    let parent = get_value_mut(root, parent_path)?;
    match parent {
        Value::Object(map) => {
            map.insert(last_key.to_string(), new_value);
            Ok(())
        }
        Value::Array(arr) => {
            let idx: usize = last_key
                .parse()
                .map_err(|_| format!("'{}' is not a valid index", last_key))?;
            if idx < arr.len() {
                arr[idx] = new_value;
                Ok(())
            } else {
                Err(format!("Index {} out of range", idx))
            }
        }
        _ => Err(format!("Cannot set value at scalar node '{}'", parent_path)),
    }
}

/// Delete the node at `path` from the tree.
fn delete_value(root: &mut Value, path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("Cannot delete root".to_string());
    }
    let (parent_path, last_key) = split_path(path)?;
    let parent = get_value_mut(root, parent_path)?;
    match parent {
        Value::Object(map) => {
            if map.remove(last_key).is_none() {
                return Err(format!("Key '{}' not found", last_key));
            }
            Ok(())
        }
        Value::Array(arr) => {
            let idx: usize = last_key
                .parse()
                .map_err(|_| format!("'{}' is not a valid index", last_key))?;
            if idx < arr.len() {
                arr.remove(idx);
                Ok(())
            } else {
                Err(format!("Index {} out of range", idx))
            }
        }
        _ => Err("Cannot delete from scalar".to_string()),
    }
}

/// Parse a dot-notation path string into individual key parts.
fn parse_path_parts(path: &str) -> Result<Vec<String>, String> {
    if path.is_empty() {
        return Ok(vec![]);
    }
    let mut parts = Vec::new();
    // Walk character-by-character so we can handle `a.b[0].c` correctly.
    let mut current = String::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => {
                if !current.is_empty() {
                    parts.push(current.clone());
                    current.clear();
                }
            }
            '[' => {
                if !current.is_empty() {
                    parts.push(current.clone());
                    current.clear();
                }
                // Collect digits until `]`.
                let mut idx = String::new();
                for d in chars.by_ref() {
                    if d == ']' {
                        break;
                    }
                    idx.push(d);
                }
                parts.push(idx);
                // Skip optional following dot.
                if chars.peek() == Some(&'.') {
                    chars.next();
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_flatten_simple() {
        let v = json!({"a": 1, "b": "hello"});
        let tree = JsonTree::from_value(v, HashSet::new());
        // Root + 2 children
        assert_eq!(tree.visible_node_count(), 3);
    }

    #[test]
    fn test_collapse() {
        let v = json!({"a": {"x": 1}});
        let mut tree = JsonTree::from_value(v, HashSet::new());
        // root + "a" + "x" = 3
        assert_eq!(tree.visible_node_count(), 3);
        // Collapse "a" (index 1)
        tree.toggle_collapse(1);
        // root + "a" (collapsed) = 2
        assert_eq!(tree.visible_node_count(), 2);
    }

    #[test]
    fn test_split_path() {
        assert_eq!(split_path("foo.bar").unwrap(), ("foo", "bar"));
        assert_eq!(split_path("foo[2]").unwrap(), ("foo", "2"));
        assert_eq!(split_path("foo").unwrap(), ("", "foo"));
    }
}
