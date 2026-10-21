use crate::clipboard::{chunk_messages, chunk_status_line};

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffPagerKey {
    Space,
    Left,
    Right,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffPager {
    chunks: Vec<String>,
    current_index: usize,
}

#[allow(dead_code)]
impl DiffPager {
    pub(crate) fn new(diff: &str, limit: usize) -> Result<Self, String> {
        let chunks = chunk_messages(diff, limit)?;
        Ok(Self {
            chunks,
            current_index: 0,
        })
    }

    pub(crate) fn current_chunk(&self) -> &str {
        &self.chunks[self.current_index]
    }

    pub(crate) fn current_index(&self) -> usize {
        self.current_index
    }

    pub(crate) fn total_chunks(&self) -> usize {
        self.chunks.len()
    }

    pub(crate) fn is_multichunk(&self) -> bool {
        self.chunks.len() > 1
    }

    /// Handles navigation keys: Space, Left, Right.
    /// Returns true if the index changed (signaling clipboard/UI update needed).
    pub(crate) fn handle_key(&mut self, key: DiffPagerKey) -> bool {
        let total = self.chunks.len();
        if total <= 1 {
            return false;
        }

        let old_index = self.current_index;
        match key {
            DiffPagerKey::Space | DiffPagerKey::Right => {
                if self.current_index + 1 < total {
                    self.current_index += 1;
                }
            }
            DiffPagerKey::Left => {
                self.current_index = self.current_index.saturating_sub(1);
            }
        }
        self.current_index != old_index
    }

    pub(crate) fn status_line(&self) -> Option<String> {
        if self.chunks.len() > 1 {
            Some(chunk_status_line(self.current_index, self.chunks.len()))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_chunk_diff_does_not_navigate_or_show_status() {
        let mut pager = DiffPager::new("small diff", 300).unwrap();
        assert!(!pager.is_multichunk());
        assert_eq!(pager.total_chunks(), 1);
        assert_eq!(pager.current_index(), 0);
        assert_eq!(pager.current_chunk(), "small diff");
        assert_eq!(pager.status_line(), None);

        assert!(!pager.handle_key(DiffPagerKey::Space));
        assert_eq!(pager.current_index(), 0);
        assert!(!pager.handle_key(DiffPagerKey::Right));
        assert_eq!(pager.current_index(), 0);
        assert!(!pager.handle_key(DiffPagerKey::Left));
        assert_eq!(pager.current_index(), 0);
    }

    #[test]
    fn multi_chunk_diff_navigates_forward_and_backward() {
        let text = "line 1\nline 2\nline 3\nline 4\nline 5\n".repeat(20);
        let mut pager = DiffPager::new(&text, 300).unwrap();
        assert!(pager.is_multichunk());
        let total = pager.total_chunks();
        assert!(total > 2);
        assert_eq!(pager.current_index(), 0);

        // Advance with Space
        assert!(pager.handle_key(DiffPagerKey::Space));
        assert_eq!(pager.current_index(), 1);

        // Advance with Right
        assert!(pager.handle_key(DiffPagerKey::Right));
        assert_eq!(pager.current_index(), 2);

        // Retreat with Left
        assert!(pager.handle_key(DiffPagerKey::Left));
        assert_eq!(pager.current_index(), 1);

        // Retreat back to 0
        assert!(pager.handle_key(DiffPagerKey::Left));
        assert_eq!(pager.current_index(), 0);
    }

    #[test]
    fn multi_chunk_diff_clamps_at_boundaries() {
        let text = "line 1\nline 2\nline 3\nline 4\nline 5\n".repeat(15);
        let mut pager = DiffPager::new(&text, 300).unwrap();
        assert_eq!(pager.current_index(), 0);

        // Left at 0 stays at 0 and returns false
        assert!(!pager.handle_key(DiffPagerKey::Left));
        assert_eq!(pager.current_index(), 0);

        let total = pager.total_chunks();
        for _ in 0..total - 1 {
            assert!(pager.handle_key(DiffPagerKey::Space));
        }
        assert_eq!(pager.current_index(), total - 1);

        // Space and Right on last part stay on last part and return false
        assert!(!pager.handle_key(DiffPagerKey::Space));
        assert_eq!(pager.current_index(), total - 1);
        assert!(!pager.handle_key(DiffPagerKey::Right));
        assert_eq!(pager.current_index(), total - 1);
    }

    #[test]
    fn multi_chunk_diff_produces_status_line() {
        let text = "sample content\n".repeat(30);
        let mut pager = DiffPager::new(&text, 300).unwrap();
        let total = pager.total_chunks();
        assert!(total > 1);

        let status0 = pager.status_line().unwrap();
        assert!(status0.contains(&format!("(1/{total})")));

        pager.handle_key(DiffPagerKey::Space);
        let status1 = pager.status_line().unwrap();
        assert!(status1.contains(&format!("(2/{total})")));
    }
}
