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
