//! Where Ctrl+Tab goes.
//!
//! The first Tab of a press of Ctrl goes to the tab that was in front before
//! this one, so a quick Ctrl+Tab flips between the last two. Pressing Tab again
//! with Ctrl still held walks along the row from there, a tab a press, and
//! backwards with Shift. Letting go of Ctrl makes where the walk started and
//! where it ended the last two, for the next quick Ctrl+Tab.

/// A tab, by an id that stays its own while it moves along the row.
pub type TabId = u64;

#[derive(Debug, Default)]
pub struct Switcher {
    /// Tabs by when they were last in front, most recent first.
    recent: Vec<TabId>,
    /// The tab that was in front when the first Tab of this press of Ctrl came,
    /// while Ctrl is still held.
    walk_from: Option<TabId>,
}

impl Switcher {
    /// `tab` came to the front some other way than by Ctrl+Tab: clicked, or
    /// opened.
    pub fn brought_forward(&mut self, tab: TabId) {
        if self.walk_from.is_none() {
            self.touch(tab);
        }
    }

    pub fn closed(&mut self, tab: TabId) {
        self.recent.retain(|&t| t != tab);
        if self.walk_from == Some(tab) {
            self.walk_from = None;
        }
    }

    /// Tab, with Ctrl held: the tab that comes to the front. `row` is the tabs
    /// in order and `front` the one in front now.
    pub fn tab_pressed(&mut self, row: &[TabId], front: TabId, backwards: bool) -> Option<TabId> {
        if row.len() < 2 {
            return None;
        }
        if self.walk_from.is_none() {
            self.walk_from = Some(front);
            let last = self
                .recent
                .iter()
                .copied()
                .find(|&t| t != front && row.contains(&t));
            if last.is_some() {
                return last;
            }
            // Nothing has been in front but this: there is no last tab to go
            // back to, so the walk starts straight away.
        }
        let at = row.iter().position(|&t| t == front)?;
        let n = row.len();
        Some(
            row[if backwards {
                (at + n - 1) % n
            } else {
                (at + 1) % n
            }],
        )
    }

    /// Ctrl was let go with `front` in front.
    pub fn released(&mut self, front: TabId) {
        if let Some(start) = self.walk_from.take() {
            self.touch(start);
            self.touch(front);
        }
    }

    /// Of the tabs in `row`, the one most recently in front: where to go when
    /// the tab in front closes.
    pub fn last_in_front(&self, row: &[TabId]) -> Option<TabId> {
        self.recent.iter().copied().find(|t| row.contains(t))
    }

    /// Whether Tab has been pressed since Ctrl went down.
    pub fn walking(&self) -> bool {
        self.walk_from.is_some()
    }

    fn touch(&mut self, tab: TabId) {
        self.recent.retain(|&t| t != tab);
        self.recent.insert(0, tab);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROW: [TabId; 4] = [10, 20, 30, 40];

    /// Brought forward in this order, so 30 is in front and 20 was before it.
    fn switcher() -> Switcher {
        let mut s = Switcher::default();
        for tab in [10, 40, 20, 30] {
            s.brought_forward(tab);
        }
        s
    }

    /// Ctrl+Tab, and Ctrl let go, from `front`: where it went.
    fn quick(s: &mut Switcher, front: TabId) -> TabId {
        let to = s.tab_pressed(&ROW, front, false).expect("a tab");
        s.released(to);
        to
    }

    #[test]
    fn a_quick_ctrl_tab_flips_between_the_last_two_tabs() {
        let mut s = switcher();
        assert_eq!(quick(&mut s, 30), 20);
        assert_eq!(quick(&mut s, 20), 30);
        assert_eq!(quick(&mut s, 30), 20);
    }

    #[test]
    fn holding_ctrl_walks_along_the_row_from_the_last_tab() {
        let mut s = switcher();
        let mut front = 30;
        let mut seen = Vec::new();
        for _ in 0..4 {
            front = s.tab_pressed(&ROW, front, false).expect("a tab");
            seen.push(front);
        }
        assert_eq!(
            seen,
            [20, 30, 40, 10],
            "the last tab, then along the row, wrapping"
        );
        s.released(front);
        assert_eq!(
            quick(&mut s, 10),
            30,
            "letting go makes where the walk started the last tab"
        );
    }

    #[test]
    fn shift_walks_the_other_way() {
        let mut s = switcher();
        let first = s.tab_pressed(&ROW, 30, true).expect("a tab");
        assert_eq!(first, 20, "the first press is the last tab either way");
        assert_eq!(s.tab_pressed(&ROW, first, true), Some(10));
        assert_eq!(s.tab_pressed(&ROW, 10, true), Some(40), "wrapping");
    }

    #[test]
    fn with_no_last_tab_the_first_press_walks() {
        let mut s = Switcher::default();
        assert_eq!(s.tab_pressed(&ROW, 20, false), Some(30));
        assert!(s.walking());
    }

    #[test]
    fn a_closed_tab_is_not_gone_back_to() {
        let mut s = switcher();
        s.closed(20);
        let row = [10, 30, 40];
        assert_eq!(
            s.tab_pressed(&row, 30, false),
            Some(40),
            "40 was in front before 20"
        );
        assert_eq!(Switcher::default().tab_pressed(&[10], 10, false), None);
    }

    #[test]
    fn closing_the_tab_in_front_goes_back_to_the_one_before_it() {
        let mut s = switcher();
        s.closed(30);
        assert_eq!(s.last_in_front(&[10, 20, 40]), Some(20));
        assert_eq!(Switcher::default().last_in_front(&[10]), None);
    }

    #[test]
    fn a_click_while_walking_does_not_disturb_the_walk() {
        let mut s = switcher();
        let to = s.tab_pressed(&ROW, 30, false).expect("a tab");
        s.brought_forward(40);
        s.released(to);
        assert_eq!(quick(&mut s, to), 30);
    }
}
