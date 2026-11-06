// Copyright (c) 2026 tre4surehunter9

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::vec;

/// A single node in the filesystem — either a file or a directory.
pub enum Node {
    File {
        name: String,
        data: Vec<u8>,
    },
    Dir {
        name: String,
        /// Indices into the Fdfs::nodes Vec for each child.
        children: Vec<usize>,
    },
}

impl Node {
    /// Returns the name of this node regardless of type.
    pub fn name(&self) -> &str {
        match self {
            Node::File { name, .. } => name,
            Node::Dir  { name, .. } => name,
        }
    }

    pub fn is_dir(&self) -> bool {
        matches!(self, Node::Dir { .. })
    }

    pub fn is_file(&self) -> bool {
        matches!(self, Node::File { .. })
    }
}

/// The filesystem itself. Holds all nodes in a flat Vec.
/// Node 0 is always the root directory.
pub struct Fdfs {
    nodes: Vec<Node>,
}

impl Fdfs {
    /// Create a new empty filesystem with a root directory.
    pub fn new() -> Self {
        let root = Node::Dir {
            name: "/".to_string(),
            children: Vec::new(),
        };
        Fdfs { nodes: vec![root] }
    }

    /// Resolve an absolute path like "/documents/hello.txt" to a node index.
    /// Returns None if any component of the path doesn't exist.
    pub fn resolve(&self, path: &str) -> Option<usize> {
        // Root always resolves to index 0
        if path == "/" {
            return Some(0);
        }

        // Split "/documents/hello.txt" into ["documents", "hello.txt"]
        // trim_start_matches removes the leading slash
        let parts: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

        let mut current = 0usize; // start at root
        for part in parts {
            let idx = self.find_child(current, part)?;
            current = idx;
        }
        Some(current)
    }

    /// Find a child of a directory node by name. Case-sensitive.
    fn find_child(&self, dir_idx: usize, name: &str) -> Option<usize> {
        if let Node::Dir { children, .. } = &self.nodes[dir_idx] {
            for &child_idx in children {
                if self.nodes[child_idx].name() == name {
                    return Some(child_idx);
                }
            }
        }
        None
    }

    /// List the contents of a directory. Returns (name, is_dir) pairs.
    pub fn list_dir(&self, path: &str) -> Result<Vec<(String, bool)>, &'static str> {
        let idx = self.resolve(path).ok_or("Directory not found")?;
        match &self.nodes[idx] {
            Node::Dir { children, .. } => {
                let mut entries = Vec::new();
                for &child_idx in children {
                    let node = &self.nodes[child_idx];
                    entries.push((node.name().to_string(), node.is_dir()));
                }
                Ok(entries)
            }
            Node::File { .. } => Err("Not a directory"),
        }
    }

    /// Read the contents of a file as a String.
    pub fn read_file(&self, path: &str) -> Result<String, &'static str> {
        let idx = self.resolve(path).ok_or("File not found")?;
        match &self.nodes[idx] {
            Node::File { data, .. } => {
                String::from_utf8(data.clone()).map_err(|_| "File is not valid UTF-8")
            }
            Node::Dir { .. } => Err("Is a directory"),
        }
    }

    /// Write a string to a file. Creates the file if it doesn't exist,
    /// overwrites it if it does. The parent directory must already exist.
    pub fn write_file(&mut self, path: &str, contents: &str) -> Result<(), &'static str> {
        // Split path into parent dir and filename
        // e.g. "/documents/hello.txt" -> ("/documents", "hello.txt")
        let (parent_path, filename) = split_path(path)?;

        let parent_idx = self.resolve(parent_path).ok_or("Parent directory not found")?;

        // Check that parent is actually a directory
        if !self.nodes[parent_idx].is_dir() {
            return Err("Parent is not a directory");
        }

        // If file already exists, overwrite it
        if let Some(existing_idx) = self.find_child(parent_idx, filename) {
            match &mut self.nodes[existing_idx] {
                Node::File { data, .. } => {
                    *data = contents.as_bytes().to_vec();
                    return Ok(());
                }
                Node::Dir { .. } => return Err("Is a directory"),
            }
        }

        // File doesn't exist — create it
        let new_idx = self.nodes.len();
        self.nodes.push(Node::File {
            name: filename.to_string(),
                        data: contents.as_bytes().to_vec(),
        });

        // Add to parent's children list
        if let Node::Dir { children, .. } = &mut self.nodes[parent_idx] {
            children.push(new_idx);
        }

        Ok(())
    }

    /// Create a directory. The parent must already exist.
    pub fn make_dir(&mut self, path: &str) -> Result<(), &'static str> {
        let (parent_path, dirname) = split_path(path)?;

        let parent_idx = self.resolve(parent_path).ok_or("Parent directory not found")?;

        if !self.nodes[parent_idx].is_dir() {
            return Err("Parent is not a directory");
        }

        // Don't create if it already exists
        if self.find_child(parent_idx, dirname).is_some() {
            return Err("Already exists");
        }

        let new_idx = self.nodes.len();
        self.nodes.push(Node::Dir {
            name: dirname.to_string(),
                        children: Vec::new(),
        });

        if let Node::Dir { children, .. } = &mut self.nodes[parent_idx] {
            children.push(new_idx);
        }

        Ok(())
    }

    /// Remove a file or empty directory.
    pub fn remove(&mut self, path: &str) -> Result<(), &'static str> {
        let (parent_path, name) = split_path(path)?;

        let parent_idx = self.resolve(parent_path).ok_or("Parent not found")?;
        let target_idx = self.find_child(parent_idx, name).ok_or("Not found")?;

        // Don't remove non-empty directories
        if let Node::Dir { children, .. } = &self.nodes[target_idx] {
            if !children.is_empty() {
                return Err("Directory is not empty");
            }
        }

        // Remove from parent's children list
        if let Node::Dir { children, .. } = &mut self.nodes[parent_idx] {
            children.retain(|&idx| idx != target_idx);
        }

        // We leave the node in the Vec as a tombstone to avoid
        // invalidating other indices. For a hobby OS this is fine.
        // A production FS would compact the Vec or use a free list.

        Ok(())
    }

    // ---- node-level access, used by file descriptors ----
    // Removed nodes stay in the Vec as tombstones, so an index taken while a
    // file was open keeps working after it is unlinked (as POSIX requires).

    pub fn node(&self, idx: usize) -> Option<&Node> {
        self.nodes.get(idx)
    }

    /// Create an empty file and return its index. Fails if the path exists.
    pub fn create_file(&mut self, path: &str) -> Result<usize, &'static str> {
        let (parent_path, filename) = split_path(path)?;
        let parent_idx = self.resolve(parent_path).ok_or("Parent directory not found")?;
        if !self.nodes[parent_idx].is_dir() {
            return Err("Parent is not a directory");
        }
        if self.find_child(parent_idx, filename).is_some() {
            return Err("Already exists");
        }
        let new_idx = self.nodes.len();
        self.nodes.push(Node::File { name: filename.to_string(), data: Vec::new() });
        if let Node::Dir { children, .. } = &mut self.nodes[parent_idx] {
            children.push(new_idx);
        }
        Ok(new_idx)
    }

    /// Size of a file in bytes (None if idx isn't a file).
    pub fn file_size(&self, idx: usize) -> Option<usize> {
        match self.nodes.get(idx)? {
            Node::File { data, .. } => Some(data.len()),
            Node::Dir { .. } => None,
        }
    }

    /// Read from a file at `offset`. Returns the number of bytes copied
    /// (0 at or past the end of the file).
    pub fn read_at(&self, idx: usize, offset: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        match self.nodes.get(idx).ok_or("Not found")? {
            Node::File { data, .. } => {
                if offset >= data.len() {
                    return Ok(0);
                }
                let n = buf.len().min(data.len() - offset);
                buf[..n].copy_from_slice(&data[offset..offset + n]);
                Ok(n)
            }
            Node::Dir { .. } => Err("Is a directory"),
        }
    }

    /// Write to a file at `offset`, growing it (zero-filled) if needed.
    pub fn write_at(&mut self, idx: usize, offset: usize, bytes: &[u8]) -> Result<usize, &'static str> {
        match self.nodes.get_mut(idx).ok_or("Not found")? {
            Node::File { data, .. } => {
                let end = offset.checked_add(bytes.len()).ok_or("File too large")?;
                if data.len() < end {
                    data.resize(end, 0);
                }
                data[offset..end].copy_from_slice(bytes);
                Ok(bytes.len())
            }
            Node::Dir { .. } => Err("Is a directory"),
        }
    }

    /// Shrink or grow a file to exactly `len` bytes.
    pub fn truncate(&mut self, idx: usize, len: usize) -> Result<(), &'static str> {
        match self.nodes.get_mut(idx).ok_or("Not found")? {
            Node::File { data, .. } => {
                data.resize(len, 0);
                Ok(())
            }
            Node::Dir { .. } => Err("Is a directory"),
        }
    }

    /// (name, node index, is_dir) for each entry of a directory node.
    pub fn dir_entries(&self, idx: usize) -> Result<Vec<(String, usize, bool)>, &'static str> {
        match self.nodes.get(idx).ok_or("Not found")? {
            Node::Dir { children, .. } => Ok(children
                .iter()
                .map(|&c| (self.nodes[c].name().to_string(), c, self.nodes[c].is_dir()))
                .collect()),
            Node::File { .. } => Err("Not a directory"),
        }
    }

    /// Check whether a path exists and is a directory.
    pub fn is_dir(&self, path: &str) -> bool {
        match self.resolve(path) {
            Some(idx) => self.nodes[idx].is_dir(),
            None => false,
        }
    }
}

/// Split "/documents/hello.txt" into ("/documents", "hello.txt").
/// Split "/hello.txt" into ("/", "hello.txt").
/// Returns an error if the path has no filename component.
fn split_path(path: &str) -> Result<(&str, &str), &'static str> {
    // Find the last '/' in the path
    let last_slash = path.rfind('/').ok_or("Invalid path")?;

    let parent = if last_slash == 0 {
        "/"  // parent is root
    } else {
        &path[..last_slash]
    };

    let name = &path[last_slash + 1..];
    if name.is_empty() {
        return Err("Path ends with /");
    }

    Ok((parent, name))
}
