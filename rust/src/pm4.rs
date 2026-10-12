//! Bounded PM4 packet decoding. Register values are raw state, not validated GPU commands.
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub shader: BTreeMap<u32, u32>,
    pub context: BTreeMap<u32, u32>,
    pub user_config: BTreeMap<u32, u32>,
    pub instances: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draw {
    pub first_vertex: u32,
    pub count: u32,
    pub instances: u32,
    pub flags: u32,
    pub state: State,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Submission {
    pub final_state: State,
    pub draws: Vec<Draw>,
}

impl State {
    fn register_count(&self) -> usize {
        self.shader.len() + self.context.len() + self.user_config.len()
    }
}

/// Decode without guest-memory or GPU side effects. Failure preserves initial state.
/// A renderer must validate each snapshot before submitting any GPU work.
pub fn decode(words: &[u32], initial: &State) -> Result<Submission, String> {
    let error = |pc: usize, message: &str| format!("PM4 DWORD {pc}: {message}");
    if words.len() > 65536 {
        return Err(error(0, "submission exceeds 256 KiB limit"));
    }
    if initial.register_count() > 512 {
        return Err(error(0, "register-state limit exceeded"));
    }
    if [&initial.shader, &initial.context, &initial.user_config]
        .iter()
        .any(|bank| bank.keys().any(|&offset| offset > 0xffff))
    {
        return Err(error(0, "invalid initial register offset"));
    }
    let mut result = Submission {
        final_state: initial.clone(),
        draws: Vec::new(),
    };
    let mut pc = 0;
    while pc < words.len() {
        let header = words[pc];
        if header == 0x80000000 {
            pc += 1;
            continue;
        }
        if header >> 30 != 3 || header & 0xff != 0 {
            return Err(error(pc, "unsupported packet type or flags"));
        }
        let length = ((header >> 16) & 0x3fff) as usize + 2;
        if length > words.len() - pc {
            return Err(error(pc, "truncated packet"));
        }
        let packet = &words[pc..pc + length];
        let state = &mut result.final_state;
        match (header >> 8) & 0xff {
            0x10 => {}
            0x2f if length == 2 => state.instances = Some(packet[1]),
            opcode @ (0x69 | 0x76 | 0x79) if length >= 3 => {
                let offset = packet[1];
                if offset > 0xffff || length - 2 > (0x10000 - offset) as usize {
                    return Err(error(pc, "unsupported register index or range"));
                }
                for (index, &value) in packet[2..].iter().enumerate() {
                    let bank = match opcode {
                        0x69 => &mut state.context,
                        0x76 => &mut state.shader,
                        _ => &mut state.user_config,
                    };
                    bank.insert(offset + index as u32, value);
                    if state.register_count() > 512 {
                        return Err(error(pc, "register-state limit exceeded"));
                    }
                }
            }
            0x2d if length == 3 => {
                if packet[2] & !0x20 != 2 {
                    return Err(error(pc, "unsupported draw initiator"));
                }
                let instances = state
                    .instances
                    .ok_or_else(|| error(pc, "missing NUM_INSTANCES"))?;
                let first_vertex = *state
                    .user_config
                    .get(&0x24a)
                    .ok_or_else(|| error(pc, "missing GE_INDX_OFFSET"))?;
                if packet[1] != 0 && first_vertex.checked_add(packet[1] - 1).is_none() {
                    return Err(error(pc, "vertex range overflow"));
                }
                if result.draws.len() >= 128 {
                    return Err(error(pc, "draw snapshot limit exceeded"));
                }
                result.draws.push(Draw {
                    first_vertex,
                    count: packet[1],
                    instances,
                    flags: packet[2] & 0x20,
                    state: state.clone(),
                });
            }
            _ => return Err(error(pc, "unsupported opcode or packet length")),
        }
        pc += length;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(op: u32, payload: &[u32]) -> Vec<u32> {
        let mut words = vec![0xc0000000 | ((payload.len() as u32 - 1) << 16) | (op << 8)];
        words.extend_from_slice(payload);
        words
    }
    #[test]
    fn snapshots_capture_draw_state_without_mutating_initial() {
        let initial = State::default();
        let mut words = packet(0x2f, &[2]);
        words.extend(packet(0x79, &[0x24a, 5]));
        words.extend(packet(0x76, &[8, 17, 19]));
        words.extend(packet(0x2d, &[3, 0x22]));
        words.extend(packet(0x76, &[8, 23]));
        words.extend(packet(0x2d, &[0, 2]));
        let result = decode(&words, &initial).unwrap();
        assert_eq!(result.draws.len(), 2);
        assert_eq!(
            (
                result.draws[0].first_vertex,
                result.draws[0].count,
                result.draws[0].instances,
                result.draws[0].flags
            ),
            (5, 3, 2, 32)
        );
        assert_eq!(result.draws[0].state.shader[&8], 17);
        assert_eq!(result.draws[1].state.shader[&8], 23);
        assert_eq!(result.final_state.shader[&9], 19);
        assert_eq!(initial, State::default());
    }
    #[test]
    fn malformed_and_unbounded_submissions_fail_without_side_effects() {
        let initial = State {
            instances: Some(1),
            user_config: BTreeMap::from([(0x24a, u32::MAX)]),
            ..State::default()
        };
        let saved = initial.clone();
        for words in [
            vec![0x80000001],
            vec![0xc0017600, 0],
            packet(0x2f, &[1, 2]),
            packet(0x76, &[0xffff, 1, 2]),
            packet(0x2d, &[2, 2]),
            packet(0x2d, &[1, 3]),
            packet(0x99, &[0]),
            vec![0x80000000; 65537],
        ] {
            assert!(decode(&words, &initial).is_err());
            assert_eq!(initial, saved);
        }
        assert!(decode(&packet(0x2d, &[1, 2]), &State::default()).is_err());
        let draws = packet(0x2d, &[0, 2]).repeat(129);
        assert!(decode(&draws, &initial).is_err());
        assert_eq!(
            decode(&draws[..128 * 3], &initial).unwrap().draws.len(),
            128
        );
        assert!(decode(&packet(0x76, &[0; 514]), &initial).is_err());
        assert!(
            decode(&[0x80000000], &State::default())
                .unwrap()
                .draws
                .is_empty()
        );
    }
}
