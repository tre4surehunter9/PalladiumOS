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
    /// Permission bits that differ from the default (0o644 / 0o755).
    perms: alloc::collections::BTreeMap<usize, u32>,
    /// Modification times (seconds since 1970).
    mtimes: alloc::collections::BTreeMap<usize, u64>,
    /// File nodes that are symlinks (their data is the target).
    symlinks: alloc::collections::BTreeSet<usize>,
    /// File nodes that are Unix sockets' names (no data).
    sockets: alloc::collections::BTreeSet<usize>,
    /// (uid, gid) of nodes not owned by root.
    owners: alloc::collections::BTreeMap<usize, (u32, u32)>,
    /// Hard links: extra names (directory, name) for file nodes. A node's
    /// own name is its first; these are the others.
    links: Vec<(usize, String, usize)>,
    /// Its type in /proc/mounts ("memfs", or "sysfs" for /sys).
    kind: &'static str,
}

impl Fdfs {
    /// Create a new empty filesystem with a root directory.
    pub fn new() -> Self {
        let root = Node::Dir {
            name: "/".to_string(),
            children: Vec::new(),
        };
        Fdfs {
            nodes: vec![root],
            perms: Default::default(),
            mtimes: Default::default(),
            symlinks: Default::default(),
            sockets: Default::default(),
            owners: Default::default(),
            links: Vec::new(),
            kind: "memfs",
        }
    }

    /// An empty one named `kind` in /proc/mounts.
    pub fn named(kind: &'static str) -> Self {
        Fdfs { kind, ..Fdfs::new() }
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
    pub fn find_child(&self, dir_idx: usize, name: &str) -> Option<usize> {
        if let Node::Dir { children, .. } = &self.nodes[dir_idx] {
            for &child_idx in children {
                if self.nodes[child_idx].name() == name {
                    return Some(child_idx);
                }
            }
        }
        self.links.iter().find(|(d, n, _)| *d == dir_idx && n == name).map(|&(_, _, node)| node)
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
                .chain(self.links.iter().filter(|(d, _, _)| *d == idx).map(|(_, n, node)| (n.clone(), *node, false)))
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

// ---- as a VFS filesystem (the in-memory one: /bin, /tmp, and / without a disk) ----

use crate::mounts::{FileSystem, FileType, FsResult, FsStats, Ino, Meta};
use crate::syscall::{EEXIST, EINVAL, EISDIR, ENOENT, ENOTDIR, ENOTEMPTY};

impl Fdfs {
    /// Create `name` in directory `dir`.
    pub fn create_in(&mut self, dir: usize, name: &str, is_dir: bool) -> Result<usize, i64> {
        if !self.nodes.get(dir).is_some_and(Node::is_dir) {
            return Err(ENOTDIR);
        }
        if self.find_child(dir, name).is_some() {
            return Err(EEXIST);
        }
        let idx = self.nodes.len();
        self.nodes.push(if is_dir {
            Node::Dir { name: name.to_string(), children: Vec::new() }
        } else {
            Node::File { name: name.to_string(), data: Vec::new() }
        });
        if let Node::Dir { children, .. } = &mut self.nodes[dir] {
            children.push(idx);
        }
        Ok(idx)
    }

    /// Remove `name` from directory `dir` (the node stays as a tombstone,
    /// so files that are still open keep working).
    pub fn remove_in(&mut self, dir: usize, name: &str) -> Result<(), i64> {
        // An extra name (hard link): just that name goes.
        if let Some(i) = self.links.iter().position(|(d, n, _)| *d == dir && n == name) {
            self.links.remove(i);
            return Ok(());
        }
        let target = self.find_child(dir, name).ok_or(ENOENT)?;
        // A file's first name, with other names left: the next one takes
        // its place (the file lives on under it).
        if let Some(i) = self.links.iter().position(|&(_, _, node)| node == target) {
            let (new_dir, new_name, _) = self.links.remove(i);
            if let Node::Dir { children, .. } = &mut self.nodes[dir] {
                children.retain(|&c| c != target);
            }
            if let Node::File { name, .. } = &mut self.nodes[target] {
                *name = new_name;
            }
            if let Node::Dir { children, .. } = &mut self.nodes[new_dir] {
                children.push(target);
            }
            return Ok(());
        }
        if let Node::Dir { children, .. } = &self.nodes[target] {
            if !children.is_empty() {
                return Err(ENOTEMPTY);
            }
        }
        if let Node::Dir { children, .. } = &mut self.nodes[dir] {
            children.retain(|&c| c != target);
        }
        Ok(())
    }
}

impl FileSystem for Fdfs {
    fn name(&self) -> &'static str {
        self.kind
    }

    fn root(&self) -> Ino {
        0
    }

    fn lookup(&mut self, dir: Ino, name: &str) -> FsResult<Ino> {
        match self.nodes.get(dir as usize) {
            Some(Node::Dir { .. }) => self.find_child(dir as usize, name).map(|i| i as Ino).ok_or(ENOENT),
            Some(Node::File { .. }) => Err(ENOTDIR),
            None => Err(ENOENT),
        }
    }

    fn meta(&mut self, ino: Ino) -> FsResult<Meta> {
        let i = ino as usize;
        let (kind, size, perm, nlink) = match self.nodes.get(i).ok_or(ENOENT)? {
            Node::File { data, .. } if self.symlinks.contains(&i) => (FileType::Symlink, data.len() as u64, 0o777, 1),
            Node::File { .. } if self.sockets.contains(&i) => (FileType::Socket, 0, 0o755, 1),
            Node::File { data, .. } => {
                let names = 1 + self.links.iter().filter(|&&(_, _, node)| node == i).count() as u32;
                (FileType::File, data.len() as u64, 0o644, names)
            }
            Node::Dir { .. } => (FileType::Dir, 0, 0o755, 2),
        };
        let perm = self.perms.get(&i).copied().unwrap_or(perm);
        let mtime = self.mtimes.get(&i).copied().unwrap_or(0);
        let (uid, gid) = self.owners.get(&i).copied().unwrap_or((0, 0));
        Ok(Meta { ino, kind, size, perm, nlink, mtime, uid, gid })
    }

    fn read(&mut self, ino: Ino, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        self.read_at(ino as usize, offset as usize, buf).map_err(|_| EISDIR)
    }

    fn write(&mut self, ino: Ino, offset: u64, data: &[u8]) -> FsResult<usize> {
        let n = self.write_at(ino as usize, offset as usize, data).map_err(|_| EISDIR)?;
        self.mtimes.insert(ino as usize, crate::rtc::now());
        Ok(n)
    }

    fn truncate(&mut self, ino: Ino, len: u64) -> FsResult<()> {
        Fdfs::truncate(self, ino as usize, len as usize).map_err(|_| EISDIR)?;
        self.mtimes.insert(ino as usize, crate::rtc::now());
        Ok(())
    }

    fn readdir(&mut self, dir: Ino) -> FsResult<Vec<(String, Ino, FileType)>> {
        let entries = self.dir_entries(dir as usize).map_err(|_| ENOTDIR)?;
        Ok(entries
            .into_iter()
            .map(|(name, idx, is_dir)| {
                let kind = if is_dir {
                    FileType::Dir
                } else if self.symlinks.contains(&idx) {
                    FileType::Symlink
                } else if self.sockets.contains(&idx) {
                    FileType::Socket
                } else {
                    FileType::File
                };
                (name, idx as Ino, kind)
            })
            .collect())
    }

    fn create(&mut self, dir: Ino, name: &str, kind: FileType, perm: u32) -> FsResult<Ino> {
        let idx = self.create_in(dir as usize, name, kind == FileType::Dir)?;
        if kind == FileType::Socket {
            self.sockets.insert(idx);
        } else {
            self.sockets.remove(&idx);
        }
        self.perms.insert(idx, perm & 0o7777);
        self.mtimes.insert(idx, crate::rtc::now());
        Ok(idx as Ino)
    }

    fn symlink(&mut self, dir: Ino, name: &str, target: &str) -> FsResult<Ino> {
        let idx = self.create_in(dir as usize, name, false)?;
        self.write_at(idx, 0, target.as_bytes()).map_err(|_| EISDIR)?;
        self.symlinks.insert(idx);
        self.mtimes.insert(idx, crate::rtc::now());
        Ok(idx as Ino)
    }

    fn readlink(&mut self, ino: Ino) -> FsResult<String> {
        let i = ino as usize;
        match self.nodes.get(i) {
            Some(Node::File { data, .. }) if self.symlinks.contains(&i) => Ok(String::from_utf8_lossy(data).into_owned()),
            Some(_) => Err(EINVAL),
            None => Err(ENOENT),
        }
    }

    fn chmod(&mut self, ino: Ino, perm: u32) -> FsResult<()> {
        if self.nodes.get(ino as usize).is_none() {
            return Err(ENOENT);
        }
        self.perms.insert(ino as usize, perm & 0o7777);
        Ok(())
    }

    fn set_mtime(&mut self, ino: Ino, mtime: u64) -> FsResult<()> {
        self.mtimes.insert(ino as usize, mtime);
        Ok(())
    }

    fn chown(&mut self, ino: Ino, uid: u32, gid: u32) -> FsResult<()> {
        if self.nodes.get(ino as usize).is_none() {
            return Err(ENOENT);
        }
        self.owners.insert(ino as usize, (uid, gid));
        Ok(())
    }

    fn rename(&mut self, old_dir: Ino, old_name: &str, new_dir: Ino, new_name: &str) -> FsResult<()> {
        let (old_dir, new_dir) = (old_dir as usize, new_dir as usize);
        let src = self.find_child(old_dir, old_name).ok_or(ENOENT)?;
        if let Some(existing) = self.find_child(new_dir, new_name) {
            if existing == src {
                return Ok(()); // two names of one file: nothing to do (as on Linux)
            }
            self.remove_in(new_dir, new_name)?; // ENOTEMPTY for a non-empty directory
        }
        // An extra name (hard link) moves on its own.
        if let Some(l) = self.links.iter_mut().find(|(d, n, _)| *d == old_dir && n == old_name) {
            *l = (new_dir, new_name.to_string(), src);
            return Ok(());
        }
        if let Node::Dir { children, .. } = &mut self.nodes[old_dir] {
            children.retain(|&c| c != src);
        }
        match &mut self.nodes[src] {
            Node::File { name, .. } | Node::Dir { name, .. } => *name = new_name.to_string(),
        }
        if let Node::Dir { children, .. } = &mut self.nodes[new_dir] {
            children.push(src);
        }
        Ok(())
    }

    fn statfs(&mut self) -> FsStats {
        let used: u64 = self
            .nodes
            .iter()
            .map(|n| match n {
                Node::File { data, .. } => data.len().div_ceil(4096) as u64,
                Node::Dir { .. } => 1,
            })
            .sum();
        // Memory-backed: report the space as what's in use (no fixed size).
        FsStats { block_size: 4096, blocks: used, free_blocks: 0, files: self.nodes.len() as u64, free_files: 0 }
    }

    fn remove(&mut self, dir: Ino, name: &str, _is_dir: bool) -> FsResult<()> {
        self.remove_in(dir as usize, name)
    }

    /// A hard link: another name for a file (not a directory).
    fn link(&mut self, dir: Ino, name: &str, target: Ino) -> FsResult<()> {
        let (dir, target) = (dir as usize, target as usize);
        match self.nodes.get(target) {
            Some(Node::File { .. }) => {}
            Some(Node::Dir { .. }) => return Err(crate::syscall::EPERM),
            None => return Err(ENOENT),
        }
        if !self.nodes.get(dir).is_some_and(Node::is_dir) {
            return Err(ENOTDIR);
        }
        if self.find_child(dir, name).is_some() {
            return Err(EEXIST);
        }
        self.links.push((dir, name.to_string(), target));
        self.mtimes.insert(target, crate::rtc::now());
        Ok(())
    }
}
