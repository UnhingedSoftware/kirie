use std::sync::Arc;

use crate::backend::{FrameSlot, PointerState};

use super::client::SharedSize;

pub type BrowserId = u64;

#[derive(Debug)]
pub struct BrowserEntry<B> {
    pub browser: B,
    pub size: Arc<SharedSize>,
    slot: FrameSlot,
    pointer: PointerState,
    last_left: bool,
    last_right: bool,
    pending_props: Vec<String>,
    audio: Vec<f32>,
    pending_media: Vec<(crate::feed::MediaChannel, String)>,
}

impl<B> BrowserEntry<B> {
    fn new(browser: B, size: Arc<SharedSize>, slot: FrameSlot) -> Self {
        Self {
            browser,
            size,
            slot,
            pointer: PointerState::default(),
            last_left: false,
            last_right: false,
            pending_props: Vec::new(),
            audio: Vec::new(),
            pending_media: Vec::new(),
        }
    }

    pub fn set_pointer(&mut self, pointer: PointerState) {
        self.pointer = pointer;
    }

    #[must_use]
    pub fn pointer(&self) -> PointerState {
        self.pointer
    }

    pub fn left_edge(&mut self) -> Option<bool> {
        (self.pointer.left != self.last_left).then(|| {
            self.last_left = self.pointer.left;
            self.pointer.left
        })
    }

    pub fn right_edge(&mut self) -> Option<bool> {
        (self.pointer.right != self.last_right).then(|| {
            self.last_right = self.pointer.right;
            self.pointer.right
        })
    }

    pub fn push_props(&mut self, json: String) {
        self.pending_props.push(json);
    }

    pub fn drain_props_if_painted(&mut self) -> Vec<String> {
        if self.pending_props.is_empty() || self.slot.load_full().is_none() {
            return Vec::new();
        }
        std::mem::take(&mut self.pending_props)
    }

    pub fn set_audio(&mut self, bands: Vec<f32>) {
        self.audio = bands;
    }

    #[must_use]
    pub fn audio(&self) -> &[f32] {
        &self.audio
    }

    pub fn push_media(&mut self, channel: crate::feed::MediaChannel, json: String) {
        self.pending_media.push((channel, json));
    }

    pub fn drain_media_if_painted(&mut self) -> Vec<(crate::feed::MediaChannel, String)> {
        if self.pending_media.is_empty() || self.slot.load_full().is_none() {
            return Vec::new();
        }
        std::mem::take(&mut self.pending_media)
    }
}

#[derive(Debug)]
pub struct BrowserRegistry<B> {
    next_id: BrowserId,
    entries: Vec<(BrowserId, BrowserEntry<B>)>,
}

impl<B> BrowserRegistry<B> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_id: 1,
            entries: Vec::new(),
        }
    }

    pub fn insert(&mut self, browser: B, size: Arc<SharedSize>, slot: FrameSlot) -> BrowserId {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push((id, BrowserEntry::new(browser, size, slot)));
        id
    }

    pub fn remove(&mut self, id: BrowserId) -> Option<BrowserEntry<B>> {
        let idx = self.entries.iter().position(|(i, _)| *i == id)?;
        Some(self.entries.remove(idx).1)
    }

    pub fn get_mut(&mut self, id: BrowserId) -> Option<&mut BrowserEntry<B>> {
        self.entries.iter_mut().find(|(i, _)| *i == id).map(|(_, e)| e)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (BrowserId, &mut BrowserEntry<B>)> {
        self.entries.iter_mut().map(|(id, e)| (*id, e))
    }

    pub fn drain(&mut self) -> impl Iterator<Item = (BrowserId, BrowserEntry<B>)> + '_ {
        self.entries.drain(..)
    }
}

impl<B> Default for BrowserRegistry<B> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::backend::{FrameBuffer, PixelFormat};

    fn size() -> Arc<SharedSize> {
        SharedSize::new(100, 100)
    }

    fn slot() -> FrameSlot {
        Arc::new(arc_swap::ArcSwapOption::empty())
    }

    fn paint(slot: &FrameSlot) {
        slot.store(Some(Arc::new(FrameBuffer {
            data: vec![0; 4],
            width: 1,
            height: 1,
            format: PixelFormat::Bgra8,
        })));
    }

    #[test]
    fn ids_are_distinct_and_monotonic() {
        let mut reg: BrowserRegistry<u8> = BrowserRegistry::new();
        let a = reg.insert(0, size(), slot());
        let b = reg.insert(1, size(), slot());
        let c = reg.insert(2, size(), slot());
        assert!(a < b && b < c);
    }

    #[test]
    fn ids_are_never_reused_after_removal() {
        let mut reg: BrowserRegistry<u8> = BrowserRegistry::new();
        let a = reg.insert(0, size(), slot());
        assert!(reg.remove(a).is_some());
        let b = reg.insert(1, size(), slot());
        assert_ne!(a, b, "a freed id must not be reallocated");
    }

    #[test]
    fn remove_targets_only_the_requested_entry() {
        let mut reg: BrowserRegistry<u8> = BrowserRegistry::new();
        let a = reg.insert(10, size(), slot());
        let b = reg.insert(20, size(), slot());
        let removed = reg.remove(a).expect("entry a");
        assert_eq!(removed.browser, 10);
        assert!(reg.get_mut(a).is_none());
        assert_eq!(reg.get_mut(b).map(|e| e.browser), Some(20));
        assert!(!reg.is_empty());
    }

    #[test]
    fn remove_twice_is_a_noop() {
        let mut reg: BrowserRegistry<u8> = BrowserRegistry::new();
        let a = reg.insert(0, size(), slot());
        assert!(reg.remove(a).is_some());
        assert!(reg.remove(a).is_none());
        assert!(reg.is_empty());
    }

    #[test]
    fn iteration_preserves_insertion_order() {
        let mut reg: BrowserRegistry<u8> = BrowserRegistry::new();
        let a = reg.insert(1, size(), slot());
        let b = reg.insert(2, size(), slot());
        let ids: Vec<BrowserId> = reg.iter_mut().map(|(id, _)| id).collect();
        assert_eq!(ids, vec![a, b]);
    }

    #[test]
    fn per_entry_size_is_independent() {
        let mut reg: BrowserRegistry<u8> = BrowserRegistry::new();
        let a = reg.insert(0, SharedSize::new(640, 480), slot());
        let b = reg.insert(1, SharedSize::new(1920, 1080), slot());
        reg.get_mut(a).unwrap().size.set(800, 600);
        assert_eq!(reg.get_mut(a).unwrap().size.width(), 800);
        assert_eq!(reg.get_mut(b).unwrap().size.width(), 1920);
    }

    #[test]
    fn pointer_edges_fire_once_per_transition() {
        let mut reg: BrowserRegistry<u8> = BrowserRegistry::new();
        let a = reg.insert(0, size(), slot());
        let entry = reg.get_mut(a).unwrap();

        assert_eq!(entry.left_edge(), None);
        assert_eq!(entry.right_edge(), None);

        entry.set_pointer(PointerState {
            x: 5,
            y: 6,
            left: true,
            right: false,
        });
        assert_eq!(entry.left_edge(), Some(true));
        assert_eq!(entry.left_edge(), None);
        assert_eq!(entry.right_edge(), None);

        entry.set_pointer(PointerState {
            x: 5,
            y: 6,
            left: false,
            right: true,
        });
        assert_eq!(entry.left_edge(), Some(false));
        assert_eq!(entry.right_edge(), Some(true));
        assert_eq!(entry.left_edge(), None);
        assert_eq!(entry.right_edge(), None);
    }

    #[test]
    fn props_stay_queued_until_first_paint() {
        let mut reg: BrowserRegistry<u8> = BrowserRegistry::new();
        let s = slot();
        let a = reg.insert(0, size(), s.clone());
        let entry = reg.get_mut(a).unwrap();

        entry.push_props("{\"a\":{\"value\":1}}".to_owned());
        assert!(entry.drain_props_if_painted().is_empty());
        assert!(entry.drain_props_if_painted().is_empty());

        paint(&s);
        assert_eq!(
            entry.drain_props_if_painted(),
            vec!["{\"a\":{\"value\":1}}".to_owned()]
        );
        assert!(entry.drain_props_if_painted().is_empty());
    }

    #[test]
    fn props_preserve_order_and_late_singles_flow_through() {
        let mut reg: BrowserRegistry<u8> = BrowserRegistry::new();
        let s = slot();
        let a = reg.insert(0, size(), s.clone());
        let entry = reg.get_mut(a).unwrap();

        entry.push_props("init".to_owned());
        entry.push_props("single-1".to_owned());
        paint(&s);
        assert_eq!(
            entry.drain_props_if_painted(),
            vec!["init".to_owned(), "single-1".to_owned()],
            "the init batch is delivered before later singles"
        );

        entry.push_props("single-2".to_owned());
        assert_eq!(entry.drain_props_if_painted(), vec!["single-2".to_owned()]);
    }

    #[test]
    fn props_paint_gate_is_per_browser() {
        let mut reg: BrowserRegistry<u8> = BrowserRegistry::new();
        let slot_a = slot();
        let a = reg.insert(0, size(), slot_a.clone());
        let b = reg.insert(1, size(), slot());

        reg.get_mut(a).unwrap().push_props("for-a".to_owned());
        reg.get_mut(b).unwrap().push_props("for-b".to_owned());

        paint(&slot_a);
        assert_eq!(
            reg.get_mut(a).unwrap().drain_props_if_painted(),
            vec!["for-a".to_owned()]
        );
        assert!(reg.get_mut(b).unwrap().drain_props_if_painted().is_empty());
    }

    #[test]
    fn pointer_position_is_stored_verbatim() {
        let mut reg: BrowserRegistry<u8> = BrowserRegistry::new();
        let a = reg.insert(0, size(), slot());
        let entry = reg.get_mut(a).unwrap();
        entry.set_pointer(PointerState {
            x: -3,
            y: 99,
            left: false,
            right: false,
        });
        let p = entry.pointer();
        assert_eq!((p.x, p.y), (-3, 99));
    }
}
