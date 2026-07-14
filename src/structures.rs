use std::path::{Component, Path, PathBuf};

// ──────────────────────────────────────────────
// PathTrie — Trie for efficient path prefix matching
// ──────────────────────────────────────────────

#[derive(Clone, Debug)]
struct PathTrieNode {
    children: Vec<(String, PathTrieNode)>,
    is_end: bool,
}

impl PathTrieNode {
    const fn new() -> Self {
        Self {
            children: Vec::new(),
            is_end: false,
        }
    }

    fn get_child(&self, component: &str) -> Option<&PathTrieNode> {
        self.children
            .iter()
            .find(|(name, _)| name == component)
            .map(|(_, node)| node)
    }

    fn get_or_insert_child(&mut self, component: &str) -> &mut PathTrieNode {
        let pos = self.children.iter().position(|(name, _)| name == component);
        match pos {
            Some(i) => &mut self.children[i].1,
            None => {
                self.children
                    .push((component.to_string(), PathTrieNode::new()));
                &mut self.children.last_mut().unwrap().1
            }
        }
    }
}

/// A trie-based data structure for efficient path prefix matching.
///
/// Allows fast checking of whether a given path is under any of the
/// inserted prefix paths.
#[derive(Clone, Debug)]
pub struct PathTrie {
    root: PathTrieNode,
}

impl PathTrie {
    pub const fn new() -> Self {
        Self {
            root: PathTrieNode::new(),
        }
    }

    /// Insert a path into the trie. Path components are split and
    /// stored as a chain. All intermediate nodes become valid prefix
    /// endpoints so that e.g. inserting `/a/b` also makes `/a` valid.
    pub fn insert(&mut self, path: &Path) {
        let mut current = &mut self.root;
        let comps: Vec<Component> = path.components().collect();
        if comps.is_empty() {
            current.is_end = true;
            return;
        }
        for component in &comps {
            match component {
                Component::RootDir => {
                    current = current.get_or_insert_child("/");
                }
                Component::Normal(name) => {
                    current = current.get_or_insert_child(&name.to_string_lossy());
                }
                Component::CurDir => {}
                Component::ParentDir => {}
                Component::Prefix(_) => {
                    current = current.get_or_insert_child(&component.as_os_str().to_string_lossy());
                }
            }
        }
        current.is_end = true;
    }

    /// Check if a path is under any prefix stored in the trie.
    /// Returns true if `path` has the same prefix as an inserted path.
    pub fn contains(&self, path: &Path) -> bool {
        let mut current = &self.root;
        let mut matched = current.is_end;

        let comps: Vec<Component> = path.components().collect();
        if comps.is_empty() {
            return matched;
        }
        for component in &comps {
            match component {
                Component::RootDir => match current.get_child("/") {
                    Some(child) => {
                        current = child;
                        if child.is_end {
                            matched = true;
                        }
                    }
                    None => return matched,
                },
                Component::Normal(name) => match current.get_child(&name.to_string_lossy()) {
                    Some(child) => {
                        current = child;
                        if child.is_end {
                            matched = true;
                        }
                    }
                    None => return matched,
                },
                Component::CurDir => {}
                Component::ParentDir => return false,
                Component::Prefix(_) => {
                    match current.get_child(&component.as_os_str().to_string_lossy()) {
                        Some(child) => {
                            current = child;
                            if child.is_end {
                                matched = true;
                            }
                        }
                        None => return matched,
                    }
                }
            }
        }

        matched
    }

    /// Find the longest matching prefix for a given path.
    pub fn longest_prefix(&self, path: &Path) -> Option<PathBuf> {
        let mut current = &self.root;
        let mut result = PathBuf::new();
        let mut last_match = if current.is_end {
            Some(result.clone())
        } else {
            None
        };

        for component in path.components() {
            match component {
                Component::RootDir => {
                    if let Some(child) = current.get_child("/") {
                        result.push("/");
                        current = child;
                        if child.is_end {
                            last_match = Some(result.clone());
                        }
                    } else {
                        break;
                    }
                }
                Component::Normal(name) => {
                    let name_str = name.to_string_lossy();
                    if let Some(child) = current.get_child(&name_str) {
                        result.push(name_str.as_ref());
                        current = child;
                        if child.is_end {
                            last_match = Some(result.clone());
                        }
                    } else {
                        break;
                    }
                }
                Component::CurDir => {}
                Component::ParentDir => break,
                Component::Prefix(_) => {
                    let name_str = component.as_os_str().to_string_lossy();
                    if let Some(child) = current.get_child(&name_str) {
                        result.push(name_str.as_ref());
                        current = child;
                        if child.is_end {
                            last_match = Some(result.clone());
                        }
                    } else {
                        break;
                    }
                }
            }
        }

        last_match
    }

    /// Insert multiple paths into the trie.
    pub fn extend(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for path in paths {
            self.insert(&path);
        }
    }
}

impl Default for PathTrie {
    fn default() -> Self {
        Self::new()
    }
}

// ──────────────────────────────────────────────
// RingBuffer — Fixed-size circular buffer
// ──────────────────────────────────────────────

/// A fixed-capacity ring buffer (circular buffer).
/// When full, new items overwrite the oldest items.
#[derive(Clone, Debug)]
pub struct RingBuffer<T> {
    buffer: Vec<Option<T>>,
    head: usize,
    size: usize,
    capacity: usize,
}

impl<T> RingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        let mut buffer = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            buffer.push(None);
        }
        Self {
            buffer,
            head: 0,
            size: 0,
            capacity,
        }
    }

    pub fn push(&mut self, item: T) {
        if self.size == self.capacity {
            self.buffer[self.head] = Some(item);
            self.head = (self.head + 1) % self.capacity;
        } else {
            let idx = (self.head + self.size) % self.capacity;
            self.buffer[idx] = Some(item);
            self.size += 1;
        }
    }

    pub fn pop(&mut self) -> Option<T> {
        if self.size == 0 {
            return None;
        }
        let item = self.buffer[self.head].take();
        self.head = (self.head + 1) % self.capacity;
        self.size -= 1;
        item
    }

    pub const fn len(&self) -> usize {
        self.size
    }

    pub const fn is_empty(&self) -> bool {
        self.size == 0
    }

    pub const fn is_full(&self) -> bool {
        self.size == self.capacity
    }

    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Collect all items in order (oldest first) into a Vec of references.
    pub fn to_vec(&self) -> Vec<&T> {
        let mut result = Vec::with_capacity(self.size);
        for i in 0..self.size {
            let idx = (self.head + i) % self.capacity;
            if let Some(ref item) = self.buffer[idx] {
                result.push(item);
            }
        }
        result
    }

    /// Consume the buffer and return items in order (oldest first).
    pub fn into_vec(mut self) -> Vec<T> {
        let mut result = Vec::with_capacity(self.size);
        while let Some(item) = self.pop() {
            result.push(item);
        }
        result
    }

    pub const fn iter(&self) -> RingBufferIter<'_, T> {
        RingBufferIter {
            buffer: self,
            index: 0,
        }
    }
}

impl<'a, T> IntoIterator for &'a RingBuffer<T> {
    type Item = &'a T;
    type IntoIter = RingBufferIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub struct RingBufferIter<'a, T> {
    buffer: &'a RingBuffer<T>,
    index: usize,
}

impl<'a, T> Iterator for RingBufferIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.buffer.size {
            return None;
        }
        let idx = (self.buffer.head + self.index) % self.buffer.capacity;
        self.index += 1;
        self.buffer.buffer[idx].as_ref()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.buffer.size.saturating_sub(self.index);
        (remaining, Some(remaining))
    }
}

// ──────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_path_trie_basic() {
        let mut trie = PathTrie::new();
        trie.insert(Path::new("/home/user/projects"));

        assert!(trie.contains(Path::new("/home/user/projects")));
        assert!(trie.contains(Path::new("/home/user/projects/src")));
        assert!(trie.contains(Path::new("/home/user/projects/src/main.rs")));
        assert!(!trie.contains(Path::new("/home/user")));
        assert!(!trie.contains(Path::new("/etc")));
    }

    #[test]
    fn test_path_trie_multiple() {
        let mut trie = PathTrie::new();
        trie.insert(Path::new("/home/user/projects"));
        trie.insert(Path::new("/var/log"));

        assert!(trie.contains(Path::new("/home/user/projects/mcp")));
        assert!(trie.contains(Path::new("/var/log/syslog")));
        assert!(!trie.contains(Path::new("/home/user")));
        assert!(!trie.contains(Path::new("/var")));
    }

    #[test]
    fn test_ring_buffer() {
        let mut buf = RingBuffer::new(3);
        assert!(buf.is_empty());

        buf.push(1);
        buf.push(2);
        buf.push(3);
        assert!(buf.is_full());
        assert_eq!(buf.to_vec(), vec![&1, &2, &3]);

        buf.push(4);
        assert_eq!(buf.to_vec(), vec![&2, &3, &4]);

        assert_eq!(buf.pop(), Some(2));
        assert_eq!(buf.pop(), Some(3));
        assert_eq!(buf.pop(), Some(4));
        assert!(buf.is_empty());
    }

    #[test]
    fn test_ring_buffer_iterator() {
        let mut buf = RingBuffer::new(3);
        buf.push(10);
        buf.push(20);
        buf.push(30);

        let collected: Vec<&i32> = buf.iter().collect();
        assert_eq!(collected, vec![&10, &20, &30]);
    }

    #[test]
    fn test_longest_prefix() {
        let mut trie = PathTrie::new();
        trie.insert(Path::new("/home/user/projects"));
        trie.insert(Path::new("/home/user"));

        let lp = trie.longest_prefix(Path::new("/home/user/projects/mcp/src"));
        assert!(lp.is_some());
        assert_eq!(lp.unwrap(), PathBuf::from("/home/user/projects"));
    }
}
