// client/src-tauri/src/voice/jitter.rs
//
// Per-peer jitter buffer: 3-frame ring keyed by `seq` from the sealed frame
// header. Pop returns Some(packet) at the head slot or None for "play PLC".

pub const JITTER_DEPTH: usize = 3;

pub struct JitterBuffer {
    slots: [Option<Vec<u8>>; JITTER_DEPTH],
    head_seq: u64,
    initialized: bool,
    popped: bool,
}

impl JitterBuffer {
    pub fn new() -> Self {
        Self {
            slots: [None, None, None],
            head_seq: 0,
            initialized: false,
            popped: false,
        }
    }

    /// Insert a packet. Returns true if accepted, false if dropped (stale/dup).
    pub fn insert(&mut self, seq: u64, packet: Vec<u8>) -> bool {
        if !self.initialized {
            self.head_seq = seq;
            self.initialized = true;
        }
        // Before any pop has occurred, allow sliding the window backward to
        // accommodate out-of-order arrivals (no pop = no committed head).
        if !self.popped && seq < self.head_seq {
            let slide_back = (self.head_seq - seq) as usize;
            if slide_back < JITTER_DEPTH {
                // Shift existing slots forward to make room at the front.
                for i in (slide_back..JITTER_DEPTH).rev() {
                    self.slots[i] = self.slots[i - slide_back].take();
                }
                for i in 0..slide_back {
                    self.slots[i] = None;
                }
                self.head_seq = seq;
            } else {
                return false; // too far back even before first pop — drop
            }
        }
        if seq < self.head_seq {
            return false; // stale (after pops have advanced the window)
        }
        let offset = (seq - self.head_seq) as usize;
        if offset < JITTER_DEPTH {
            if self.slots[offset].is_some() {
                return false; // duplicate
            }
            self.slots[offset] = Some(packet);
            true
        } else {
            // Far future — advance head to (seq - JITTER_DEPTH + 1), dropping old.
            let advance = offset - JITTER_DEPTH + 1;
            self.advance_head(advance);
            self.slots[JITTER_DEPTH - 1] = Some(packet);
            true
        }
    }

    /// Pop the head slot. Advances the window. Returns None for "loss → PLC".
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        if !self.initialized {
            return None;
        }
        self.popped = true;
        let out = self.slots[0].take();
        self.advance_head(1);
        out
    }

    fn advance_head(&mut self, n: usize) {
        if n >= JITTER_DEPTH {
            self.slots = [None, None, None];
        } else {
            for i in 0..(JITTER_DEPTH - n) {
                self.slots[i] = self.slots[i + n].take();
            }
            // The tail slots that were shifted from are now None already
            // due to .take(); make this explicit for the last `n` slots.
            for i in (JITTER_DEPTH - n)..JITTER_DEPTH {
                self.slots[i] = None;
            }
        }
        self.head_seq = self.head_seq.saturating_add(n as u64);
    }
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_order_insert_then_pop_returns_packets() {
        let mut jb = JitterBuffer::new();
        assert!(jb.insert(10, vec![1]));
        assert!(jb.insert(11, vec![2]));
        assert!(jb.insert(12, vec![3]));
        assert_eq!(jb.pop(), Some(vec![1]));
        assert_eq!(jb.pop(), Some(vec![2]));
        assert_eq!(jb.pop(), Some(vec![3]));
    }

    #[test]
    fn out_of_order_inserts_reorder() {
        let mut jb = JitterBuffer::new();
        assert!(jb.insert(12, vec![3]));
        assert!(jb.insert(10, vec![1]));
        assert!(jb.insert(11, vec![2]));
        assert_eq!(jb.pop(), Some(vec![1]));
        assert_eq!(jb.pop(), Some(vec![2]));
        assert_eq!(jb.pop(), Some(vec![3]));
    }

    #[test]
    fn stale_packet_is_dropped() {
        let mut jb = JitterBuffer::new();
        assert!(jb.insert(10, vec![1]));
        let _ = jb.pop(); // head_seq advances to 11
        let _ = jb.pop(); // head_seq advances to 12
        assert!(!jb.insert(9, vec![99]), "seq below head must be rejected");
    }

    #[test]
    fn duplicate_packet_is_dropped() {
        let mut jb = JitterBuffer::new();
        assert!(jb.insert(10, vec![1]));
        assert!(!jb.insert(10, vec![99]), "duplicate seq must be rejected");
    }

    #[test]
    fn gap_yields_none_for_plc() {
        let mut jb = JitterBuffer::new();
        assert!(jb.insert(10, vec![1]));
        assert!(jb.insert(12, vec![3]));
        assert_eq!(jb.pop(), Some(vec![1]));
        assert_eq!(jb.pop(), None, "seq 11 missing → PLC");
        assert_eq!(jb.pop(), Some(vec![3]));
    }

    #[test]
    fn far_future_seq_advances_window_dropping_older_slots() {
        let mut jb = JitterBuffer::new();
        assert!(jb.insert(10, vec![1]));
        assert!(jb.insert(11, vec![2]));
        assert!(jb.insert(100, vec![100]));
        // After far-future insert, head_seq = 98, slot[2] = the new packet.
        // Popping should yield None, None, then the packet.
        assert_eq!(jb.pop(), None);
        assert_eq!(jb.pop(), None);
        assert_eq!(jb.pop(), Some(vec![100]));
    }

    #[test]
    fn empty_buffer_pop_returns_none_without_panic() {
        let mut jb = JitterBuffer::new();
        assert_eq!(jb.pop(), None);
    }
}
