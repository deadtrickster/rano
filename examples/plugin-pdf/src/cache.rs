//! **The page cache: the last few pages, explicitly, in one place.** Three
//! entries, LRU-evicted, keyed by document and page — small on purpose: moving
//! back one page is instant while the rest of the document stays unrendered.
//!
//! **Only text is kept**: a page's picture is up to 16 MiB the host wrote
//! straight into the arena where `provider_picture` answers it, and the render
//! ladder asks for a different dpi at every step, so no picture entry would
//! ever be hit. A page of text is kilobytes, and it is what search, copy and
//! the view itself are made of.
//!
//! The ABI instantiates a fresh module per call, so this cache is empty at the
//! start of each call: within one call it saves a repeat ask the view would
//! otherwise make. The moment a host keeps one instance across a view's calls
//! this is already the reader's page cache and no other line moves — which is
//! why it is a `static` with a fixed capacity rather than a local.

use std::sync::Mutex;

/// How many pages are kept: the page, the one behind it and one ahead.
pub const PAGES: usize = 3;

/// What a cached entry is keyed by. **`doc` is part of the key**, a hash of
/// the document's path — a cache keyed only on the page number would show one
/// document's page 3 in another document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub doc: u64,
    pub page: u32,
}

impl Key {
    /// A page of a document.
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

    /// **A hit, which also makes the entry the most recent.** `None` for a miss.
    pub fn get(&mut self, key: Key) -> Option<&String> {
        let at = self.slots.iter().position(|(k, _)| *k == key)?;
        let slot = self.slots.remove(at);
        self.slots.insert(0, slot);
        self.slots.first().map(|(_, text)| text)
    }

    /// **A page, whether or not it was already there.** Evicts the least recently used.
    pub fn put(&mut self, key: Key, text: String) {
        self.slots.retain(|(k, _)| *k != key);
        self.slots.insert(0, (key, text));
        self.slots.truncate(self.cap);
    }
}

/// **A document's identity in the cache**: FNV-1a over the path, because a key
/// is compared and copied on every lookup and a path is a `String` that would
/// be cloned into every entry.
pub fn id(path: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// The one cache this module has.
static CACHE: Mutex<Cache> = Mutex::new(Cache::new(PAGES));

/// **Empty the cache**: a host that reloads a plugin, or opens a different
/// document in the same instance, must not read the last one's pages.
pub fn clear() {
    with(|c| {
        c.slots.clear();
    });
}

/// **Run `f` with the cache.** A poisoned lock is not a reason to lose a page:
/// the guard is taken back out of the poison and the call goes on (a panic in
/// a plugin is a trap the host reports; the cache must not be the second thing
/// that goes wrong).
pub fn with<R>(f: impl FnOnce(&mut Cache) -> R) -> R {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}
