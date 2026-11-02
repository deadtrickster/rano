//! The page cache: the last few pages, LRU-evicted, keyed by document and page, so
//! going back one page is instant while the rest stays unrendered. Text only: a
//! picture is up to 16 MiB the host writes, at a dpi the ladder changes each step.

use std::sync::Mutex;

/// How many pages are kept: the page, the one behind it and one ahead.
pub const PAGES: usize = 3;

/// What a cached entry is keyed by: `doc` is part of it, or one document's page 3
/// would show in another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub doc: u64,
    pub page: u32,
}

impl Key {
    pub fn new(doc: u64, page: u32) -> Key {
        Key { doc, page }
    }
}

/// The cache itself: most recently used first.
#[derive(Debug)]
pub struct Cache {
    cap: usize,
    slots: Vec<(Key, String)>,
}

impl Cache {
    pub const fn new(cap: usize) -> Cache {
        Cache {
            cap,
            slots: Vec::new(),
        }
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The keys held, most recently used first — what a test reads to pin the order.
    pub fn keys(&self) -> Vec<Key> {
        self.slots.iter().map(|(k, _)| *k).collect()
    }

    /// A hit, which also makes the entry the most recent; `None` for a miss.
    pub fn get(&mut self, key: Key) -> Option<&String> {
        let at = self.slots.iter().position(|(k, _)| *k == key)?;
        let slot = self.slots.remove(at);
        self.slots.insert(0, slot);
        self.slots.first().map(|(_, text)| text)
    }

    /// A page, whether or not it was already there; evicts the least recently used.
    pub fn put(&mut self, key: Key, text: String) {
        self.slots.retain(|(k, _)| *k != key);
        self.slots.insert(0, (key, text));
        self.slots.truncate(self.cap);
    }
}

/// A document's identity in the cache: FNV-1a over the path, because a key is
/// copied on every lookup and a `String` path would be cloned into every entry.
pub fn id(path: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// The one cache this module has: a `static`, so a host that keeps one instance
/// across a view's calls already has the reader's page cache.
static CACHE: Mutex<Cache> = Mutex::new(Cache::new(PAGES));

/// Empty the cache: a host that reloads a plugin, or opens a different document in
/// the same instance, must not read the last one's pages.
pub fn clear() {
    with(|c| {
        c.slots.clear();
    });
}

/// Run `f` with the cache. A poisoned lock is not a reason to lose a page: the
/// guard is taken back out of the poison and the call goes on — a panic is a trap
/// the host reports, and the cache must not be the second thing to go wrong.
pub fn with<R>(f: impl FnOnce(&mut Cache) -> R) -> R {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}
