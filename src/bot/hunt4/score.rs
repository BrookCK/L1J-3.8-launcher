use crate::bot::hunt4::step::TargetCandidate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetScoreSummary {
    pub rank: usize,
    pub target_id: u32,
    pub entity_ptr: u32,
    pub name: String,
    pub tile: (i32, i32),
    pub distance: u32,
    pub in_attack_range: bool,
    pub reachable: bool,
    pub is_attacker: bool,
    pub approach_steps: Option<usize>,
    pub approach_next_tile: Option<(i32, i32)>,
    pub reason: String,
}

pub fn summarize_ranked_candidates(candidates: &[TargetCandidate]) -> Vec<TargetScoreSummary> {
    let mut scores: Vec<_> = candidates.iter().map(summary_from_candidate).collect();
    for (idx, score) in scores.iter_mut().enumerate() {
        score.rank = idx + 1;
    }
    scores
}

fn summary_from_candidate(candidate: &TargetCandidate) -> TargetScoreSummary {
    let approach_steps = candidate.reachable_path.as_ref().map(Vec::len);
    let approach_next_tile = candidate
        .reachable_path
        .as_ref()
        .and_then(|path| path.first().copied());
    let reachable = approach_steps.is_some();
    TargetScoreSummary {
        rank: 0,
        target_id: candidate.target_id,
        entity_ptr: candidate.entity_ptr,
        name: candidate.name.clone(),
        tile: candidate.tile,
        distance: candidate.distance,
        in_attack_range: candidate.in_attack_range,
        reachable,
        is_attacker: candidate.is_attacker,
        approach_steps,
        approach_next_tile,
        reason: reason_for(candidate, reachable, approach_steps),
    }
}

fn reason_for(
    candidate: &TargetCandidate,
    reachable: bool,
    approach_steps: Option<usize>,
) -> String {
    let mut parts = Vec::new();
    parts.push(if reachable {
        "reachable".to_string()
    } else {
        "unreachable".to_string()
    });
    if candidate.is_attacker {
        parts.push("attacker".to_string());
    }
    if candidate.in_attack_range {
        parts.push("in_range".to_string());
    }
    if let Some(steps) = approach_steps {
        parts.push(format!("approach_steps={steps}"));
    }
    parts.push(format!("distance={}", candidate.distance));
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use crate::bot::hunt4::step::TargetCandidate;
    use crate::bot::hunt4::targeting;

    fn candidate(
        target_id: u32,
        distance: u32,
        in_attack_range: bool,
        reachable_path: Option<Vec<(i32, i32)>>,
        is_attacker: bool,
    ) -> TargetCandidate {
        TargetCandidate {
            target_id,
            entity_ptr: target_id + 0x1000,
            name: format!("mob_{target_id:X}"),
            tile: (target_id as i32, target_id as i32 + 1),
            distance,
            in_attack_range,
            reachable_path,
            is_attacker,
        }
    }

    #[test]
    fn summarize_ranked_candidates_uses_targeting_order_and_adds_rank() {
        let mut candidates = vec![
            candidate(0x0100_0001, 2, true, Some(Vec::new()), false),
            candidate(
                0x0100_0002,
                7,
                false,
                Some(vec![(1, 1), (2, 2), (3, 3)]),
                true,
            ),
            candidate(0x0100_0003, 1, false, None, false),
        ];
        targeting::sort_candidates(&mut candidates);

        let scores = super::summarize_ranked_candidates(&candidates);

        assert_eq!(
            scores
                .iter()
                .map(|score| score.target_id)
                .collect::<Vec<_>>(),
            vec![0x0100_0001, 0x0100_0002, 0x0100_0003]
        );
        assert_eq!(scores[0].rank, 1);
        assert!(scores[0].reason.contains("in_range"));
        assert!(scores[1].reason.contains("attacker"));
        assert!(scores[1].reason.contains("approach_steps=3"));
        assert_eq!(scores[2].approach_steps, None);
    }

    #[test]
    fn summarize_ranked_candidates_preserves_input_order_from_planner() {
        let scores = super::summarize_ranked_candidates(&[
            candidate(
                0x0100_0002,
                7,
                false,
                Some(vec![(1, 1), (2, 2), (3, 3)]),
                true,
            ),
            candidate(0x0100_0001, 2, true, Some(Vec::new()), false),
        ]);

        assert_eq!(
            scores
                .iter()
                .map(|score| (score.rank, score.target_id))
                .collect::<Vec<_>>(),
            vec![(1, 0x0100_0002), (2, 0x0100_0001)]
        );
    }

    #[test]
    fn summarize_candidates_records_first_approach_tile() {
        let scores = super::summarize_ranked_candidates(&[candidate(
            0x0100_0001,
            4,
            false,
            Some(vec![(101, 100), (102, 100)]),
            false,
        )]);

        assert_eq!(scores[0].approach_next_tile, Some((101, 100)));
    }
}
