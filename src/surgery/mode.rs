#![allow(dead_code)]

use std::time::Instant;

use crate::surgery::find_begin_token;
use crate::surgery::keys::{Event, ARM_TIMEOUT};
use crate::surgery::pager::{DiffPager, DiffPagerKey};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CandidateData {
    pub id: String,
    pub files_count: usize,
    pub diff: String,
    pub pager: DiffPager,
}

const MIN_CHOOSER_CANDIDATES: usize = 2;
const MAX_CHOOSER_CANDIDATES: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChooserState {
    candidate_lines: Vec<usize>,
    selected_index: usize,
    suggested_index: Option<usize>,
    context_line_count: usize,
    scroll_offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChooserTransition {
    Changed,
    Selected(usize),
    Aborted,
    Ignored,
}

impl ChooserState {
    pub(crate) fn new(
        mut candidate_lines: Vec<usize>,
        suggested_index: Option<usize>,
    ) -> Option<Self> {
        if !(MIN_CHOOSER_CANDIDATES..=MAX_CHOOSER_CANDIDATES)
            .contains(&candidate_lines.len())
            || candidate_lines.contains(&0)
            || suggested_index.is_some_and(|index| index >= candidate_lines.len())
        {
            return None;
        }

        if let Some(index) = suggested_index {
            let suggested_line = candidate_lines.remove(index);
            candidate_lines.insert(0, suggested_line);
        }

        Some(Self {
            candidate_lines,
            selected_index: 0,
            suggested_index: suggested_index.map(|_| 0),
            context_line_count: 1,
            scroll_offset: 0,
        })
    }

    pub(crate) fn selected_line(&self) -> usize {
        self.candidate_lines[self.selected_index]
    }

    pub(crate) fn suggested_line(&self) -> Option<usize> {
        self.suggested_index.map(|index| self.candidate_lines[index])
    }

    pub(crate) fn candidate_lines(&self) -> &[usize] {
        &self.candidate_lines
    }

    pub(crate) fn set_context_line_count(&mut self, line_count: usize) {
        self.context_line_count = line_count.max(1);
        self.scroll_offset = self
            .scroll_offset
            .min(self.context_line_count.saturating_sub(1));
    }

    pub(crate) fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub(crate) fn focus_line(&mut self, line: usize) {
        let offset = line.saturating_sub(8);
        self.scroll_to(offset);
    }

    fn scroll_to(&mut self, offset: usize) -> ChooserTransition {
        let next = offset.min(self.context_line_count.saturating_sub(1));
        if next == self.scroll_offset {
            ChooserTransition::Ignored
        } else {
            self.scroll_offset = next;
            ChooserTransition::Changed
        }
    }

    pub(crate) fn step(&mut self, event: Event) -> ChooserTransition {
        match event {
            Event::Left => {
                let previous = self.selected_index;
                self.move_left();
                if self.selected_index != previous {
                    ChooserTransition::Changed
                } else {
                    ChooserTransition::Ignored
                }
            }
            Event::Right => {
                let previous = self.selected_index;
                self.move_right();
                if self.selected_index != previous {
                    ChooserTransition::Changed
                } else {
                    ChooserTransition::Ignored
                }
            }
            Event::ChooseCandidate(index) if index < self.candidate_lines.len() => {
                let previous = self.selected_index;
                self.selected_index = index;
                if self.selected_index == previous {
                    ChooserTransition::Ignored
                } else {
                    ChooserTransition::Changed
                }
            }
            Event::ChooseCandidate(_) => ChooserTransition::Ignored,
            Event::Up => self.scroll_to(self.scroll_offset.saturating_sub(1)),
            Event::Down => self.scroll_to(self.scroll_offset.saturating_add(1)),
            Event::PageUp => self.scroll_to(self.scroll_offset.saturating_sub(15)),
            Event::PageDown => self.scroll_to(self.scroll_offset.saturating_add(15)),
            Event::Home => self.scroll_to(0),
            Event::End => self.scroll_to(self.context_line_count.saturating_sub(1)),
            Event::Enter => ChooserTransition::Selected(self.selected_line()),
            Event::Abort | Event::Eof => ChooserTransition::Aborted,
            _ => ChooserTransition::Ignored,
        }
    }

    pub(crate) fn move_left(&mut self) {
        self.selected_index = self.selected_index.saturating_sub(1);
    }

    pub(crate) fn move_right(&mut self) {
        self.selected_index =
            (self.selected_index + 1).min(self.candidate_lines.len() - 1);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModeState {
    Idle,
    Candidate(CandidateData),
    Armed {
        candidate: CandidateData,
        armed_at: Instant,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModeAction {
    ReadClipboard,
    RunDryRun(String),
    IgnoreNonEnvelope { message: String },
    ArmBuild,
    FireBuild,
    Disarm,
    NothingToBuild,
    UpdatePagerChunk {
        chunk: String,
        status_line: Option<String>,
    },
    ReportDryRunError(String),
    Abort { outcome: String },
}

pub(crate) fn step(
    state: ModeState,
    event: Event,
    now: Instant,
    session_token: &str,
) -> (ModeState, Vec<ModeAction>) {
    match state {
        ModeState::Idle => match event {
            Event::Build => (ModeState::Idle, vec![ModeAction::NothingToBuild]),
            Event::Abort | Event::Eof => (
                ModeState::Idle,
                vec![ModeAction::Abort {
                    outcome: "CVB surgery ABORTED (no envelope)\n".to_string(),
                }],
            ),
            Event::PasteKey => (ModeState::Idle, vec![ModeAction::ReadClipboard]),
            Event::Paste(text) => handle_paste(ModeState::Idle, &text, session_token),
            Event::PasteError(message) => (
                ModeState::Idle,
                vec![ModeAction::IgnoreNonEnvelope { message }],
            ),
            _ => (ModeState::Idle, Vec::new()),
        },
        ModeState::Candidate(candidate) => match event {
            Event::Build => (
                ModeState::Armed {
                    candidate,
                    armed_at: now,
                },
                vec![ModeAction::ArmBuild],
            ),
            Event::Abort | Event::Eof => (
                ModeState::Idle,
                vec![ModeAction::Abort {
                    outcome: format!("CVB surgery ABORTED id={}\n", candidate.id),
                }],
            ),
            Event::PasteKey => (ModeState::Candidate(candidate), vec![ModeAction::ReadClipboard]),
            Event::Paste(text) => handle_paste(ModeState::Candidate(candidate), &text, session_token),
            Event::PasteError(message) => (
                ModeState::Candidate(candidate),
                vec![ModeAction::IgnoreNonEnvelope { message }],
            ),
            Event::Space => handle_pager_nav(candidate, DiffPagerKey::Space),
            Event::Left => handle_pager_nav(candidate, DiffPagerKey::Left),
            Event::Right => handle_pager_nav(candidate, DiffPagerKey::Right),
            _ => (ModeState::Candidate(candidate), Vec::new()),
        },
        ModeState::Armed {
            candidate,
            armed_at,
        } => {
            let timed_out = now.saturating_duration_since(armed_at) > ARM_TIMEOUT;
            if timed_out {
                // Timeout resets state to Candidate, then handles event
                match event {
                    Event::Build => (
                        ModeState::Armed {
                            candidate,
                            armed_at: now,
                        },
                        vec![ModeAction::ArmBuild],
                    ),
                    Event::Abort | Event::Eof => (
                        ModeState::Idle,
                        vec![ModeAction::Abort {
                            outcome: format!("CVB surgery ABORTED id={}\n", candidate.id),
                        }],
                    ),
                    Event::PasteKey => (
                        ModeState::Candidate(candidate),
                        vec![ModeAction::Disarm, ModeAction::ReadClipboard],
                    ),
                    Event::Paste(text) => {
                        let (next_state, mut actions) =
                            handle_paste(ModeState::Candidate(candidate), &text, session_token);
                        actions.insert(0, ModeAction::Disarm);
                        (next_state, actions)
                    }
                    Event::PasteError(message) => (
                        ModeState::Candidate(candidate),
                        vec![
                            ModeAction::Disarm,
                            ModeAction::IgnoreNonEnvelope { message },
                        ],
                    ),
                    Event::Space => {
                        let (next_state, mut actions) =
                            handle_pager_nav(candidate, DiffPagerKey::Space);
                        actions.insert(0, ModeAction::Disarm);
                        (next_state, actions)
                    }
                    Event::Left => {
                        let (next_state, mut actions) =
                            handle_pager_nav(candidate, DiffPagerKey::Left);
                        actions.insert(0, ModeAction::Disarm);
                        (next_state, actions)
                    }
                    Event::Right => {
                        let (next_state, mut actions) =
                            handle_pager_nav(candidate, DiffPagerKey::Right);
                        actions.insert(0, ModeAction::Disarm);
                        (next_state, actions)
                    }
                    _ => (ModeState::Candidate(candidate), vec![ModeAction::Disarm]),
                }
            } else {
                // Armed within 3 seconds
                match event {
                    Event::Build => (ModeState::Candidate(candidate), vec![ModeAction::FireBuild]),
                    Event::Abort | Event::Eof => (
                        ModeState::Idle,
                        vec![ModeAction::Abort {
                            outcome: format!("CVB surgery ABORTED id={}\n", candidate.id),
                        }],
                    ),
                    Event::PasteKey => (
                        ModeState::Candidate(candidate),
                        vec![ModeAction::Disarm, ModeAction::ReadClipboard],
                    ),
                    Event::Paste(text) => {
                        let (next_state, mut actions) =
                            handle_paste(ModeState::Candidate(candidate), &text, session_token);
                        actions.insert(0, ModeAction::Disarm);
                        (next_state, actions)
                    }
                    Event::PasteError(message) => (
                        ModeState::Candidate(candidate),
                        vec![
                            ModeAction::Disarm,
                            ModeAction::IgnoreNonEnvelope { message },
                        ],
                    ),
                    Event::Space => {
                        let (next_state, mut actions) =
                            handle_pager_nav(candidate, DiffPagerKey::Space);
                        actions.insert(0, ModeAction::Disarm);
                        (next_state, actions)
                    }
                    Event::Left => {
                        let (next_state, mut actions) =
                            handle_pager_nav(candidate, DiffPagerKey::Left);
                        actions.insert(0, ModeAction::Disarm);
                        (next_state, actions)
                    }
                    Event::Right => {
                        let (next_state, mut actions) =
                            handle_pager_nav(candidate, DiffPagerKey::Right);
                        actions.insert(0, ModeAction::Disarm);
                        (next_state, actions)
                    }
                    _ => (ModeState::Candidate(candidate), vec![ModeAction::Disarm]),
                }
            }
        }
    }
}

fn handle_paste(
    current_state: ModeState,
    text: &str,
    session_token: &str,
) -> (ModeState, Vec<ModeAction>) {
    if let Some(token) = find_begin_token(text) {
        if token != session_token {
            (
                current_state,
                vec![ModeAction::IgnoreNonEnvelope {
                    message: format!(
                        "CVB: E-TOKEN: envelope token '{token}' does not match active session token '{session_token}'; run `cvb token` to print the current token."
                    ),
                }],
            )
        } else {
            // Recognisable envelope: any previous candidate is discarded first!
            (ModeState::Idle, vec![ModeAction::RunDryRun(text.to_string())])
        }
    } else {
        (
            current_state,
            vec![ModeAction::IgnoreNonEnvelope {
                message: "clipboard does not contain an envelope".to_string(),
            }],
        )
    }
}

fn handle_pager_nav(
    mut candidate: CandidateData,
    key: DiffPagerKey,
) -> (ModeState, Vec<ModeAction>) {
    if candidate.pager.handle_key(key) {
        let chunk = candidate.pager.current_chunk().to_string();
        let status_line = candidate.pager.status_line();
        (
            ModeState::Candidate(candidate),
            vec![ModeAction::UpdatePagerChunk { chunk, status_line }],
        )
    } else {
        (ModeState::Candidate(candidate), Vec::new())
    }
}

pub(crate) fn step_dry_run_result(
    result: Result<CandidateData, String>,
) -> (ModeState, Vec<ModeAction>) {
    match result {
        Ok(candidate) => {
            let chunk = candidate.pager.current_chunk().to_string();
            let status_line = candidate.pager.status_line();
            (
                ModeState::Candidate(candidate),
                vec![ModeAction::UpdatePagerChunk { chunk, status_line }],
            )
        }
        Err(error_msg) => (ModeState::Idle, vec![ModeAction::ReportDryRunError(error_msg)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn chooser_accepts_two_through_five_candidates() {
        for count in MIN_CHOOSER_CANDIDATES..=MAX_CHOOSER_CANDIDATES {
            let lines: Vec<usize> = (1..=count).map(|line| line * 10).collect();
            assert!(ChooserState::new(lines, Some(0)).is_some());
        }
    }

    #[test]
    fn chooser_rejects_invalid_candidate_counts_and_indices() {
        assert!(ChooserState::new(vec![10], Some(0)).is_none());
        assert!(ChooserState::new(vec![10, 20, 30, 40, 50, 60], Some(0)).is_none());
        assert!(ChooserState::new(vec![0, 20], Some(0)).is_none());
        assert!(ChooserState::new(vec![10, 20], Some(2)).is_none());
    }

    #[test]
    fn chooser_suggestion_moves_to_first_slot_and_starts_selected() {
        let chooser = ChooserState::new(vec![10, 20, 30], Some(1)).unwrap();

        assert_eq!(chooser.candidate_lines(), &[20, 10, 30]);
        assert_eq!(chooser.selected_line(), 20);
        assert_eq!(chooser.suggested_line(), Some(20));
    }

    #[test]
    fn chooser_without_suggestion_preserves_order_and_starts_first() {
        let chooser = ChooserState::new(vec![10, 20, 30], None).unwrap();

        assert_eq!(chooser.candidate_lines(), &[10, 20, 30]);
        assert_eq!(chooser.selected_line(), 10);
        assert_eq!(chooser.suggested_line(), None);
    }

    #[test]
    fn chooser_navigation_keeps_suggestion_marker_fixed() {
        let mut chooser = ChooserState::new(vec![10, 20, 30], Some(1)).unwrap();

        assert_eq!(chooser.selected_line(), 20);
        assert_eq!(chooser.suggested_line(), Some(20));

        chooser.move_right();
        assert_eq!(chooser.selected_line(), 10);
        assert_eq!(chooser.suggested_line(), Some(20));

        chooser.move_right();
        assert_eq!(chooser.selected_line(), 30);
        chooser.move_left();
        assert_eq!(chooser.selected_line(), 10);
        chooser.move_left();
        assert_eq!(chooser.selected_line(), 20);
        chooser.move_left();
        assert_eq!(chooser.selected_line(), 20);

        assert_eq!(chooser.suggested_line(), Some(20));
    }

    #[test]
    fn chooser_enter_selects_the_current_line() {
        let mut chooser = ChooserState::new(vec![10, 20, 30], Some(1)).unwrap();

        assert_eq!(chooser.step(Event::Enter), ChooserTransition::Selected(20));

        chooser.step(Event::Right);
        assert_eq!(chooser.step(Event::Enter), ChooserTransition::Selected(10));
        assert_eq!(chooser.suggested_line(), Some(20));
    }

    #[test]
    fn chooser_number_keys_select_candidates_without_moving_suggestion() {
        let mut chooser = ChooserState::new(vec![10, 20, 30], Some(1)).unwrap();

        assert_eq!(
            chooser.step(Event::ChooseCandidate(2)),
            ChooserTransition::Changed
        );
        assert_eq!(chooser.selected_line(), 30);
        assert_eq!(chooser.suggested_line(), Some(20));

        assert_eq!(
            chooser.step(Event::ChooseCandidate(0)),
            ChooserTransition::Changed
        );
        assert_eq!(chooser.selected_line(), 20);
        assert_eq!(chooser.suggested_line(), Some(20));

        assert_eq!(
            chooser.step(Event::ChooseCandidate(4)),
            ChooserTransition::Ignored
        );
        assert_eq!(chooser.selected_line(), 20);
        assert_eq!(chooser.suggested_line(), Some(20));
    }

    #[test]
    fn chooser_abort_and_eof_never_select_a_candidate() {
        let mut chooser = ChooserState::new(vec![10, 20], None).unwrap();

        assert_eq!(chooser.step(Event::Abort), ChooserTransition::Aborted);
        assert_eq!(chooser.step(Event::Eof), ChooserTransition::Aborted);
        assert_eq!(chooser.selected_line(), 10);
        assert_eq!(chooser.suggested_line(), None);
    }

    #[test]
    fn chooser_reports_changes_but_ignores_boundary_and_unrelated_events() {
        let mut chooser = ChooserState::new(vec![10, 20], Some(0)).unwrap();

        assert_eq!(chooser.step(Event::Left), ChooserTransition::Ignored);
        assert_eq!(chooser.step(Event::Right), ChooserTransition::Changed);
        assert_eq!(chooser.step(Event::Right), ChooserTransition::Ignored);
        assert_eq!(chooser.step(Event::Space), ChooserTransition::Ignored);
        assert_eq!(chooser.candidate_lines(), &[10, 20]);
        assert_eq!(chooser.suggested_line(), Some(10));
    }
    #[test]
    fn chooser_context_navigation_scrolls_and_clamps() {
        let mut chooser = ChooserState::new(vec![10, 20], Some(0)).unwrap();
        chooser.set_context_line_count(40);

        assert_eq!(chooser.scroll_offset(), 0);
        assert_eq!(chooser.step(Event::Up), ChooserTransition::Ignored);
        assert_eq!(chooser.step(Event::Down), ChooserTransition::Changed);
        assert_eq!(chooser.scroll_offset(), 1);
        assert_eq!(chooser.step(Event::PageDown), ChooserTransition::Changed);
        assert_eq!(chooser.scroll_offset(), 16);
        assert_eq!(chooser.step(Event::PageUp), ChooserTransition::Changed);
        assert_eq!(chooser.scroll_offset(), 1);
        assert_eq!(chooser.step(Event::Home), ChooserTransition::Changed);
        assert_eq!(chooser.scroll_offset(), 0);
        assert_eq!(chooser.step(Event::End), ChooserTransition::Changed);
        assert_eq!(chooser.scroll_offset(), 39);
        assert_eq!(chooser.step(Event::Down), ChooserTransition::Ignored);
        assert_eq!(chooser.suggested_line(), Some(10));
        assert_eq!(chooser.selected_line(), 10);
    }

    fn sample_candidate(id: &str) -> CandidateData {
        let diff = format!("diff for {id}\n");
        let pager = DiffPager::new(&diff, 300).unwrap();
        CandidateData {
            id: id.to_string(),
            files_count: 1,
            diff,
            pager,
        }
    }

    fn sample_multichunk_candidate(id: &str) -> CandidateData {
        let diff = "line 1\nline 2\nline 3\nline 4\nline 5\n".repeat(20);
        let pager = DiffPager::new(&diff, 300).unwrap();
        assert!(pager.is_multichunk());
        CandidateData {
            id: id.to_string(),
            files_count: 1,
            diff,
            pager,
        }
    }

    #[test]
    fn idle_build_yields_nothing_to_build() {
        let now = Instant::now();
        let (state, actions) = step(ModeState::Idle, Event::Build, now, "tok123");
        assert_eq!(state, ModeState::Idle);
        assert_eq!(actions, vec![ModeAction::NothingToBuild]);
    }

    #[test]
    fn idle_abort_yields_aborted_no_envelope() {
        let now = Instant::now();
        let (state, actions) = step(ModeState::Idle, Event::Abort, now, "tok123");
        assert_eq!(state, ModeState::Idle);
        assert_eq!(
            actions,
            vec![ModeAction::Abort {
                outcome: "CVB surgery ABORTED (no envelope)\n".to_string()
            }]
        );
    }

    #[test]
    fn idle_eof_yields_aborted_no_envelope() {
        let now = Instant::now();
        let (state, actions) = step(ModeState::Idle, Event::Eof, now, "tok123");
        assert_eq!(state, ModeState::Idle);
        assert_eq!(
            actions,
            vec![ModeAction::Abort {
                outcome: "CVB surgery ABORTED (no envelope)\n".to_string()
            }]
        );
    }

    #[test]
    fn idle_space_and_arrows_are_ignored() {
        let now = Instant::now();
        for key in [Event::Space, Event::Left, Event::Right, Event::Other] {
            let (state, actions) = step(ModeState::Idle, key, now, "tok123");
            assert_eq!(state, ModeState::Idle);
            assert!(actions.is_empty());
        }
    }

    #[test]
    fn idle_paste_error_yields_ignore_non_envelope() {
        let now = Instant::now();
        let (state, actions) = step(
            ModeState::Idle,
            Event::PasteError("clipboard failure".to_string()),
            now,
            "tok123",
        );
        assert_eq!(state, ModeState::Idle);
        assert_eq!(
            actions,
            vec![ModeAction::IgnoreNonEnvelope {
                message: "clipboard failure".to_string(),
            }]
        );
    }

    #[test]
    fn candidate_paste_error_keeps_candidate_and_yields_ignore_non_envelope() {
        let now = Instant::now();
        let candidate = sample_candidate("cand1");
        let (state, actions) = step(
            ModeState::Candidate(candidate.clone()),
            Event::PasteError("clipboard failure".to_string()),
            now,
            "tok123",
        );
        assert_eq!(state, ModeState::Candidate(candidate));
        assert_eq!(
            actions,
            vec![ModeAction::IgnoreNonEnvelope {
                message: "clipboard failure".to_string(),
            }]
        );
    }

    #[test]
    fn armed_paste_error_disarms_and_yields_ignore_non_envelope() {
        let now = Instant::now();
        let candidate = sample_candidate("cand1");
        let (state, actions) = step(
            ModeState::Armed {
                candidate: candidate.clone(),
                armed_at: now,
            },
            Event::PasteError("clipboard failure".to_string()),
            now,
            "tok123",
        );
        assert_eq!(state, ModeState::Candidate(candidate));
        assert_eq!(
            actions,
            vec![
                ModeAction::Disarm,
                ModeAction::IgnoreNonEnvelope {
                    message: "clipboard failure".to_string(),
                },
            ]
        );
    }

    #[test]
    fn idle_paste_non_envelope_is_ignored_and_keeps_idle() {
        let now = Instant::now();
        let (state, actions) = step(
            ModeState::Idle,
            Event::Paste("just some random text".to_string()),
            now,
            "tok123",
        );
        assert_eq!(state, ModeState::Idle);
        assert_eq!(
            actions,
            vec![ModeAction::IgnoreNonEnvelope {
                message: "clipboard does not contain an envelope".to_string()
            }]
        );
    }

    #[test]
    fn idle_paste_wrong_token_yields_e_token_and_keeps_idle() {
        let now = Instant::now();
        let (state, actions) = step(
            ModeState::Idle,
            Event::Paste("CVB::wrong::BEGIN v=1 id=test\n".to_string()),
            now,
            "tok123",
        );
        assert_eq!(state, ModeState::Idle);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            ModeAction::IgnoreNonEnvelope { message } => {
                assert!(message.contains("E-TOKEN"));
                assert!(message.contains("wrong"));
                assert!(message.contains("tok123"));
            }
            other => panic!("expected IgnoreNonEnvelope, got {other:?}"),
        }
    }

    #[test]
    fn candidate_build_arms_with_arm_build_action() {
        let now = Instant::now();
        let candidate = sample_candidate("id-1");
        let (state, actions) = step(
            ModeState::Candidate(candidate.clone()),
            Event::Build,
            now,
            "tok123",
        );
        assert_eq!(state, ModeState::Armed { candidate, armed_at: now });
        assert_eq!(actions, vec![ModeAction::ArmBuild]);
    }

    #[test]
    fn armed_build_within_3s_fires_build() {
        let now = Instant::now();
        let candidate = sample_candidate("id-1");
        let armed = ModeState::Armed {
            candidate: candidate.clone(),
            armed_at: now,
        };
        let (state, actions) = step(
            armed,
            Event::Build,
            now + Duration::from_millis(1500),
            "tok123",
        );
        assert_eq!(state, ModeState::Candidate(candidate));
        assert_eq!(actions, vec![ModeAction::FireBuild]);
    }

    #[test]
    fn armed_build_after_3s_rearms() {
        let now = Instant::now();
        let candidate = sample_candidate("id-1");
        let armed = ModeState::Armed {
            candidate: candidate.clone(),
            armed_at: now,
        };
        let later = now + Duration::from_millis(3500);
        let (state, actions) = step(armed, Event::Build, later, "tok123");
        assert_eq!(state, ModeState::Armed { candidate, armed_at: later });
        assert_eq!(actions, vec![ModeAction::ArmBuild]);
    }

    #[test]
    fn armed_other_key_disarms_to_candidate() {
        let now = Instant::now();
        let candidate = sample_candidate("id-1");
        let armed = ModeState::Armed {
            candidate: candidate.clone(),
            armed_at: now,
        };
        let (state, actions) = step(armed, Event::Other, now, "tok123");
        assert_eq!(state, ModeState::Candidate(candidate));
        assert_eq!(actions, vec![ModeAction::Disarm]);
    }

    #[test]
    fn armed_abort_yields_aborted_with_id() {
        let now = Instant::now();
        let candidate = sample_candidate("my-patch-id");
        let armed = ModeState::Armed {
            candidate,
            armed_at: now,
        };
        let (state, actions) = step(armed, Event::Abort, now, "tok123");
        assert_eq!(state, ModeState::Idle);
        assert_eq!(
            actions,
            vec![ModeAction::Abort {
                outcome: "CVB surgery ABORTED id=my-patch-id\n".to_string()
            }]
        );
    }

    #[test]
    fn candidate_abort_yields_aborted_with_id() {
        let now = Instant::now();
        let candidate = sample_candidate("my-patch-id");
        let (state, actions) = step(ModeState::Candidate(candidate), Event::Abort, now, "tok123");
        assert_eq!(state, ModeState::Idle);
        assert_eq!(
            actions,
            vec![ModeAction::Abort {
                outcome: "CVB surgery ABORTED id=my-patch-id\n".to_string()
            }]
        );
    }

    #[test]
    fn candidate_space_and_arrows_navigate_pager_chunks() {
        let now = Instant::now();
        let candidate = sample_multichunk_candidate("multi-patch");
        assert_eq!(candidate.pager.current_index(), 0);

        // Advance with Space
        let (state, actions) = step(
            ModeState::Candidate(candidate),
            Event::Space,
            now,
            "tok123",
        );
        match state {
            ModeState::Candidate(ref c) => assert_eq!(c.pager.current_index(), 1),
            other => panic!("expected Candidate state, got {other:?}"),
        }
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            ModeAction::UpdatePagerChunk { chunk, status_line } => {
                assert!(status_line.is_some());
                assert!(chunk.contains("Part 2/"));
            }
            other => panic!("expected UpdatePagerChunk, got {other:?}"),
        }
    }

    #[test]
    fn candidate_paste_non_envelope_keeps_candidate() {
        let now = Instant::now();
        let candidate = sample_candidate("held-candidate");
        let (state, actions) = step(
            ModeState::Candidate(candidate.clone()),
            Event::Paste("not an envelope".to_string()),
            now,
            "tok123",
        );
        assert_eq!(state, ModeState::Candidate(candidate));
        assert_eq!(
            actions,
            vec![ModeAction::IgnoreNonEnvelope {
                message: "clipboard does not contain an envelope".to_string()
            }]
        );
    }

    #[test]
    fn candidate_paste_wrong_token_keeps_candidate() {
        let now = Instant::now();
        let candidate = sample_candidate("held-candidate");
        let (state, actions) = step(
            ModeState::Candidate(candidate.clone()),
            Event::Paste("CVB::wrongtok::BEGIN v=1 id=test\n".to_string()),
            now,
            "tok123",
        );
        assert_eq!(state, ModeState::Candidate(candidate));
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            ModeAction::IgnoreNonEnvelope { message } => {
                assert!(message.contains("E-TOKEN"));
                assert!(message.contains("wrongtok"));
            }
            other => panic!("expected IgnoreNonEnvelope, got {other:?}"),
        }
    }

    #[test]
    fn candidate_paste_valid_envelope_discards_candidate_and_runs_dry_run() {
        let now = Instant::now();
        let candidate = sample_candidate("old-candidate");
        let envelope_text = "CVB::tok123::BEGIN v=1 id=new-patch\n".to_string();
        let (state, actions) = step(
            ModeState::Candidate(candidate),
            Event::Paste(envelope_text.clone()),
            now,
            "tok123",
        );
        assert_eq!(state, ModeState::Idle);
        assert_eq!(actions, vec![ModeAction::RunDryRun(envelope_text)]);
    }

    #[test]
    fn dry_run_success_transitions_to_candidate_and_updates_clipboard() {
        let candidate = sample_candidate("patch-x");
        let (state, actions) = step_dry_run_result(Ok(candidate.clone()));
        assert_eq!(state, ModeState::Candidate(candidate));
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], ModeAction::UpdatePagerChunk { .. }));
    }

    #[test]
    fn dry_run_failure_transitions_to_idle_and_reports_error() {
        let (state, actions) = step_dry_run_result(Err("E-PRECOND: failed".to_string()));
        assert_eq!(state, ModeState::Idle);
        assert_eq!(
            actions,
            vec![ModeAction::ReportDryRunError("E-PRECOND: failed".to_string())]
        );
    }
}
