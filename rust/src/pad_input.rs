//! Explicit host mapping policy, not a claim about console calibration.
//! Apple normalized axes [-1,1] map to [0,255] with nearest rounding (center128),
//! and Y is inverted. Pressure maps [0,1] to [0,255]. Face/shoulder/trigger
//! buttons press at >=0.5; dpad directions press at magnitude>=0.5. No additional
//! deadzone is applied because Apple's profile already applies its deadzone.
//! A/B/X/Y map to Cross/Circle/Square/Triangle. The caller must supply a snapshot
//! from a currently observed device; this pure conversion cannot prove presence.
//! Timestamp and count are supplied by the caller without invented units.
//! Motion, touch, stick-click, menu/options and extension input are unavailable
//! in the current host snapshot and remain zero, not claimed sensor readings.
use crate::{Result, game_controller::Snapshot, pad_data::*};

pub fn map(snapshot: &Snapshot, timestamp: u64, count: u8) -> Result<PadData> {
    for &value in snapshot
        .left_stick
        .iter()
        .chain(&snapshot.right_stick)
        .chain(&snapshot.dpad)
    {
        validate(value, -1.0)?;
    }
    for &value in snapshot
        .face
        .iter()
        .chain(&snapshot.shoulders)
        .chain(&snapshot.triggers)
    {
        validate(value, 0.0)?;
    }
    let mut buttons = 0;
    for (value, flag) in snapshot
        .face
        .iter()
        .chain(&snapshot.shoulders)
        .chain(&snapshot.triggers)
        .zip([
            BUTTON_CROSS,
            BUTTON_CIRCLE,
            BUTTON_SQUARE,
            BUTTON_TRIANGLE,
            BUTTON_L1,
            BUTTON_R1,
            BUTTON_L2,
            BUTTON_R2,
        ])
    {
        if *value >= 0.5 {
            buttons |= flag;
        }
    }
    for (value, positive, negative) in [
        (snapshot.dpad[0], BUTTON_RIGHT, BUTTON_LEFT),
        (snapshot.dpad[1], BUTTON_UP, BUTTON_DOWN),
    ] {
        if value >= 0.5 {
            buttons |= positive;
        }
        if value <= -0.5 {
            buttons |= negative;
        }
    }
    Ok(PadData {
        buttons,
        left_stick: [axis(snapshot.left_stick[0]), axis(-snapshot.left_stick[1])],
        right_stick: [
            axis(snapshot.right_stick[0]),
            axis(-snapshot.right_stick[1]),
        ],
        triggers: snapshot.triggers.map(|value| (value * 255.0).round() as u8),
        connected: true,
        timestamp,
        count,
        ..PadData::default()
    })
}
fn validate(value: f32, lower: f32) -> Result<()> {
    if !value.is_finite() || !(lower..=1.0).contains(&value) {
        Err("Invalid normalized controller input".into())
    } else {
        Ok(())
    }
}
fn axis(value: f32) -> u8 {
    ((value + 1.0) * 127.5).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    fn idle() -> Snapshot {
        Snapshot {
            enumeration_index: 99,
            left_stick: [0.0; 2],
            right_stick: [0.0; 2],
            dpad: [0.0; 2],
            face: [0.0; 4],
            shoulders: [0.0; 2],
            triggers: [0.0; 2],
        }
    }
    #[test]
    fn centered_axes_y_inversion_pressure_and_caller_metadata() {
        let mut snapshot = idle();
        let neutral = map(&snapshot, 9, 3).unwrap();
        assert_eq!(neutral.left_stick, [128; 2]);
        assert_eq!(neutral.right_stick, [128; 2]);
        assert_eq!(neutral.buttons, 0);
        assert_eq!(neutral.triggers, [0; 2]);
        assert!(neutral.connected);
        assert_eq!(neutral.timestamp, 9);
        assert_eq!(neutral.count, 3);
        assert_eq!(neutral.quaternion, [0.0; 4]);
        assert_eq!(neutral.fingers, 0);
        snapshot.left_stick = [-1.0, 1.0];
        snapshot.right_stick = [1.0, -1.0];
        snapshot.triggers = [0.5, 1.0];
        let state = map(&snapshot, u64::MAX, u8::MAX).unwrap();
        assert_eq!(state.left_stick, [0, 0]);
        assert_eq!(state.right_stick, [255, 255]);
        assert_eq!(state.triggers, [128, 255]);
        assert_eq!(state.buttons, BUTTON_L2 | BUTTON_R2);
    }
    #[test]
    fn thresholds_and_conventional_button_mapping_are_explicit() {
        for (index, flag) in [
            BUTTON_CROSS,
            BUTTON_CIRCLE,
            BUTTON_SQUARE,
            BUTTON_TRIANGLE,
            BUTTON_L1,
            BUTTON_R1,
            BUTTON_L2,
            BUTTON_R2,
        ]
        .into_iter()
        .enumerate()
        {
            for (value, expected) in [(0.499, 0), (0.5, flag), (1.0, flag)] {
                let mut s = idle();
                *s.face
                    .iter_mut()
                    .chain(&mut s.shoulders)
                    .chain(&mut s.triggers)
                    .nth(index)
                    .unwrap() = value;
                assert_eq!(map(&s, 0, 0).unwrap().buttons, expected);
            }
        }
        for (dpad, expected) in [
            ([0.499, -0.499], 0),
            ([0.5, 0.5], BUTTON_RIGHT | BUTTON_UP),
            ([-0.5, -0.5], BUTTON_LEFT | BUTTON_DOWN),
        ] {
            let mut s = idle();
            s.dpad = dpad;
            assert_eq!(map(&s, 0, 0).unwrap().buttons, expected);
        }
    }
    #[test]
    fn every_input_rejects_nan_infinity_and_out_of_range_before_returning_state() {
        for index in 0..14 {
            for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1.001, -1.001] {
                let mut s = idle();
                *s.left_stick
                    .iter_mut()
                    .chain(&mut s.right_stick)
                    .chain(&mut s.dpad)
                    .chain(&mut s.face)
                    .chain(&mut s.shoulders)
                    .chain(&mut s.triggers)
                    .nth(index)
                    .unwrap() = value;
                assert!(map(&s, 0, 0).is_err());
            }
            if index >= 6 {
                let mut s = idle();
                *s.face
                    .iter_mut()
                    .chain(&mut s.shoulders)
                    .chain(&mut s.triggers)
                    .nth(index - 6)
                    .unwrap() = -0.01;
                assert!(map(&s, 0, 0).is_err());
            }
        }
    }
}
