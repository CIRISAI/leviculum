use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    AToB,
    BToA,
    Both,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Direction::AToB => write!(f, "a_to_b"),
            Direction::BToA => write!(f, "b_to_a"),
            Direction::Both => write!(f, "both"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Drop,
    Delay(u64),
    Corrupt,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Drop => write!(f, "drop"),
            Action::Delay(ms) => write!(f, "delay {ms}"),
            Action::Corrupt => write!(f, "corrupt"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Command(u8),
}

impl fmt::Display for Filter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Filter::All => write!(f, "all"),
            Filter::Command(cmd) => write!(f, "cmd:0x{cmd:02x}"),
        }
    }
}

pub struct Rule {
    pub id: u32,
    pub direction: Direction,
    pub action: Action,
    pub filter: Filter,
    /// Only match frames with payload >= this size (0 = no minimum).
    pub min_size: usize,
    /// Only match frames with payload <= this size (0 = no maximum).
    pub max_size: usize,
    /// Number of matching frames to skip before the rule activates.
    pub skip: u32,
    pub remaining: Option<u32>,
}

pub struct KissFrame {
    pub command: u8,
    pub payload: Vec<u8>,
}

pub enum FrameDecision {
    Forward,
    Drop,
    Delay(u64),
    Corrupt(Vec<u8>),
}

pub struct RuleEngine {
    rules: Vec<Rule>,
    next_id: u32,
    pub forwarded: u64,
    pub dropped: u64,
    pub delayed: u64,
    pub corrupted: u64,
}

impl Default for RuleEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl RuleEngine {
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            next_id: 1,
            forwarded: 0,
            dropped: 0,
            delayed: 0,
            corrupted: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_rule(
        &mut self,
        direction: Direction,
        action: Action,
        filter: Filter,
        min_size: usize,
        max_size: usize,
        skip: u32,
        count: Option<u32>,
    ) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.rules.push(Rule {
            id,
            direction,
            action,
            filter,
            min_size,
            max_size,
            skip,
            remaining: count,
        });
        id
    }

    pub fn clear_rule(&mut self, id: u32) -> bool {
        let len_before = self.rules.len();
        self.rules.retain(|r| r.id != id);
        self.rules.len() < len_before
    }

    pub fn clear_all(&mut self) {
        self.rules.clear();
    }

    pub fn list_rules(&self) -> &[Rule] {
        &self.rules
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Evaluate rules for a frame. First matching rule wins.
    /// If the winning rule still has skip > 0, the frame is forwarded and
    /// skip is decremented. Once skip reaches 0, the action is applied.
    /// Decrements remaining count; auto-removes rules that reach 0.
    pub fn evaluate(&mut self, frame: &KissFrame, dir: Direction) -> FrameDecision {
        let mut matched_idx = None;

        for (i, rule) in self.rules.iter().enumerate() {
            let dir_match = rule.direction == Direction::Both || rule.direction == dir;
            if !dir_match {
                continue;
            }

            let filter_match = match rule.filter {
                Filter::All => true,
                Filter::Command(cmd) => frame.command == cmd,
            };
            if !filter_match {
                continue;
            }

            if rule.min_size > 0 && frame.payload.len() < rule.min_size {
                continue;
            }

            if rule.max_size > 0 && frame.payload.len() > rule.max_size {
                continue;
            }

            matched_idx = Some(i);
            break;
        }

        let Some(idx) = matched_idx else {
            self.forwarded += 1;
            return FrameDecision::Forward;
        };

        let rule = &mut self.rules[idx];

        // If skip > 0, forward the frame and decrement skip
        if rule.skip > 0 {
            rule.skip -= 1;
            self.forwarded += 1;
            return FrameDecision::Forward;
        }

        let decision = match rule.action {
            Action::Drop => {
                self.dropped += 1;
                FrameDecision::Drop
            }
            Action::Delay(ms) => {
                self.delayed += 1;
                FrameDecision::Delay(ms)
            }
            Action::Corrupt => {
                self.corrupted += 1;
                let mut corrupted = frame.payload.clone();
                if !corrupted.is_empty() {
                    corrupted[0] ^= 0xFF;
                }
                FrameDecision::Corrupt(corrupted)
            }
        };

        // Decrement remaining count, auto-remove at 0
        if let Some(ref mut remaining) = rule.remaining {
            *remaining -= 1;
            if *remaining == 0 {
                self.rules.remove(idx);
            }
        }

        decision
    }

    pub fn stats_json(&self) -> String {
        format!(
            r#"{{"forwarded":{},"dropped":{},"delayed":{},"corrupted":{},"rules":{}}}"#,
            self.forwarded,
            self.dropped,
            self.delayed,
            self.corrupted,
            self.rules.len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_rules_forwards() {
        let mut engine = RuleEngine::new();
        let frame = KissFrame {
            command: 0x00,
            payload: vec![1, 2, 3],
        };
        assert!(matches!(
            engine.evaluate(&frame, Direction::AToB),
            FrameDecision::Forward
        ));
        assert_eq!(engine.forwarded, 1);
    }

    #[test]
    fn drop_rule() {
        let mut engine = RuleEngine::new();
        engine.add_rule(Direction::Both, Action::Drop, Filter::All, 0, 0, 0, None);
        let frame = KissFrame {
            command: 0x00,
            payload: vec![1, 2, 3],
        };
        assert!(matches!(
            engine.evaluate(&frame, Direction::AToB),
            FrameDecision::Drop
        ));
        assert_eq!(engine.dropped, 1);
        assert_eq!(engine.forwarded, 0);
    }

    #[test]
    fn drop_with_count() {
        let mut engine = RuleEngine::new();
        engine.add_rule(Direction::Both, Action::Drop, Filter::All, 0, 0, 0, Some(2));
        let frame = KissFrame {
            command: 0x00,
            payload: vec![1],
        };

        assert!(matches!(
            engine.evaluate(&frame, Direction::AToB),
            FrameDecision::Drop
        ));
        assert!(matches!(
            engine.evaluate(&frame, Direction::AToB),
            FrameDecision::Drop
        ));
        // Count exhausted, rule auto-removed
        assert!(matches!(
            engine.evaluate(&frame, Direction::AToB),
            FrameDecision::Forward
        ));
        assert_eq!(engine.dropped, 2);
        assert_eq!(engine.forwarded, 1);
        assert_eq!(engine.rule_count(), 0);
    }

    #[test]
    fn direction_filter() {
        let mut engine = RuleEngine::new();
        engine.add_rule(Direction::AToB, Action::Drop, Filter::All, 0, 0, 0, None);
        let frame = KissFrame {
            command: 0x00,
            payload: vec![1],
        };

        // A->B matches
        assert!(matches!(
            engine.evaluate(&frame, Direction::AToB),
            FrameDecision::Drop
        ));
        // B->A does not match
        assert!(matches!(
            engine.evaluate(&frame, Direction::BToA),
            FrameDecision::Forward
        ));
    }

    #[test]
    fn command_filter() {
        let mut engine = RuleEngine::new();
        engine.add_rule(
            Direction::Both,
            Action::Drop,
            Filter::Command(0x00),
            0,
            0,
            0,
            None,
        );

        let data_frame = KissFrame {
            command: 0x00,
            payload: vec![1],
        };
        let other_frame = KissFrame {
            command: 0x08,
            payload: vec![1],
        };

        assert!(matches!(
            engine.evaluate(&data_frame, Direction::AToB),
            FrameDecision::Drop
        ));
        assert!(matches!(
            engine.evaluate(&other_frame, Direction::AToB),
            FrameDecision::Forward
        ));
    }

    #[test]
    fn corrupt_flips_first_byte() {
        let mut engine = RuleEngine::new();
        engine.add_rule(Direction::Both, Action::Corrupt, Filter::All, 0, 0, 0, None);
        let frame = KissFrame {
            command: 0x00,
            payload: vec![0xAB, 0xCD],
        };

        match engine.evaluate(&frame, Direction::AToB) {
            FrameDecision::Corrupt(data) => {
                assert_eq!(data[0], 0xAB ^ 0xFF);
                assert_eq!(data[1], 0xCD); // second byte unchanged
            }
            _ => panic!("Expected Corrupt"),
        }
    }

    #[test]
    fn delay_returns_ms() {
        let mut engine = RuleEngine::new();
        engine.add_rule(
            Direction::Both,
            Action::Delay(150),
            Filter::All,
            0,
            0,
            0,
            None,
        );
        let frame = KissFrame {
            command: 0x00,
            payload: vec![1],
        };

        match engine.evaluate(&frame, Direction::AToB) {
            FrameDecision::Delay(ms) => assert_eq!(ms, 150),
            _ => panic!("Expected Delay"),
        }
        assert_eq!(engine.delayed, 1);
    }

    #[test]
    fn first_matching_rule_wins() {
        let mut engine = RuleEngine::new();
        engine.add_rule(Direction::Both, Action::Drop, Filter::All, 0, 0, 0, None);
        engine.add_rule(Direction::Both, Action::Corrupt, Filter::All, 0, 0, 0, None);
        let frame = KissFrame {
            command: 0x00,
            payload: vec![1],
        };

        // Drop rule matches first
        assert!(matches!(
            engine.evaluate(&frame, Direction::AToB),
            FrameDecision::Drop
        ));
    }

    #[test]
    fn clear_rule_by_id() {
        let mut engine = RuleEngine::new();
        let id = engine.add_rule(Direction::Both, Action::Drop, Filter::All, 0, 0, 0, None);
        assert!(engine.clear_rule(id));
        assert!(!engine.clear_rule(id)); // already removed
        assert_eq!(engine.rule_count(), 0);
    }

    #[test]
    fn clear_all() {
        let mut engine = RuleEngine::new();
        engine.add_rule(Direction::Both, Action::Drop, Filter::All, 0, 0, 0, None);
        engine.add_rule(Direction::Both, Action::Corrupt, Filter::All, 0, 0, 0, None);
        engine.clear_all();
        assert_eq!(engine.rule_count(), 0);
    }

    #[test]
    fn stats_json_format() {
        let mut engine = RuleEngine::new();
        engine.forwarded = 10;
        engine.dropped = 2;
        engine.delayed = 1;
        engine.corrupted = 3;
        engine.add_rule(Direction::Both, Action::Drop, Filter::All, 0, 0, 0, None);

        let json = engine.stats_json();
        assert_eq!(
            json,
            r#"{"forwarded":10,"dropped":2,"delayed":1,"corrupted":3,"rules":1}"#
        );
    }

    #[test]
    fn skip_forwards_before_acting() {
        let mut engine = RuleEngine::new();
        // Skip 2 matching frames, then drop the next 3
        engine.add_rule(Direction::Both, Action::Drop, Filter::All, 0, 0, 2, Some(3));
        let frame = KissFrame {
            command: 0x00,
            payload: vec![1],
        };

        // First two are forwarded (skip)
        assert!(matches!(
            engine.evaluate(&frame, Direction::AToB),
            FrameDecision::Forward
        ));
        assert!(matches!(
            engine.evaluate(&frame, Direction::AToB),
            FrameDecision::Forward
        ));
        assert_eq!(engine.forwarded, 2);
        assert_eq!(engine.dropped, 0);

        // Next three are dropped
        assert!(matches!(
            engine.evaluate(&frame, Direction::AToB),
            FrameDecision::Drop
        ));
        assert!(matches!(
            engine.evaluate(&frame, Direction::AToB),
            FrameDecision::Drop
        ));
        assert!(matches!(
            engine.evaluate(&frame, Direction::AToB),
            FrameDecision::Drop
        ));
        assert_eq!(engine.dropped, 3);

        // Rule exhausted, back to forwarding
        assert!(matches!(
            engine.evaluate(&frame, Direction::AToB),
            FrameDecision::Forward
        ));
        assert_eq!(engine.forwarded, 3);
        assert_eq!(engine.rule_count(), 0);
    }

    #[test]
    fn max_size_filter() {
        let mut engine = RuleEngine::new();
        // Drop frames with payload between 50 and 130 bytes
        engine.add_rule(Direction::Both, Action::Drop, Filter::All, 50, 130, 0, None);

        let small_frame = KissFrame {
            command: 0x00,
            payload: vec![0u8; 30],
        };
        let medium_frame = KissFrame {
            command: 0x00,
            payload: vec![0u8; 86],
        };
        let large_frame = KissFrame {
            command: 0x00,
            payload: vec![0u8; 200],
        };

        // 30 bytes: below min_size, forwarded
        assert!(matches!(
            engine.evaluate(&small_frame, Direction::AToB),
            FrameDecision::Forward
        ));
        // 86 bytes: within range, dropped
        assert!(matches!(
            engine.evaluate(&medium_frame, Direction::AToB),
            FrameDecision::Drop
        ));
        // 200 bytes: above max_size, forwarded
        assert!(matches!(
            engine.evaluate(&large_frame, Direction::AToB),
            FrameDecision::Forward
        ));

        assert_eq!(engine.dropped, 1);
        assert_eq!(engine.forwarded, 2);
    }

    #[test]
    fn max_size_zero_means_no_limit() {
        let mut engine = RuleEngine::new();
        // max_size=0 means no upper bound
        engine.add_rule(Direction::Both, Action::Drop, Filter::All, 50, 0, 0, None);

        let large_frame = KissFrame {
            command: 0x00,
            payload: vec![0u8; 10000],
        };

        // Should match since max_size=0 imposes no limit
        assert!(matches!(
            engine.evaluate(&large_frame, Direction::AToB),
            FrameDecision::Drop
        ));
    }
}
