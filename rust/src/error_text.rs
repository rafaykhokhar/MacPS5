//! Initial guest C-locale errno descriptions. Never index the host errno table.
pub fn describe(number: i32) -> (String, bool) {
    let text = match number {
        0 => "No error",
        1 => "Operation not permitted",
        2 => "No such file or directory",
        3 => "No such process",
        4 => "Interrupted system call",
        5 => "Input/output error",
        9 => "Bad file descriptor",
        12 => "Cannot allocate memory",
        13 => "Permission denied",
        14 => "Bad address",
        22 => "Invalid argument",
        32 => "Broken pipe",
        35 => "Resource temporarily unavailable",
        38 => "Socket operation on non-socket",
        42 => "Protocol not available",
        78 => "Function not implemented",
        _ => return (format!("Unknown error: {number}"), false),
    };
    (text.into(), true)
}
#[cfg(test)]
mod tests {
    #[test]
    fn guest_numbers_and_unknown_signed_values() {
        assert_eq!(
            super::describe(35),
            ("Resource temporarily unavailable".into(), true)
        );
        assert_eq!(
            super::describe(78),
            ("Function not implemented".into(), true)
        );
        assert_eq!(super::describe(-1), ("Unknown error: -1".into(), false));
        assert_eq!(
            super::describe(i32::MIN),
            ("Unknown error: -2147483648".into(), false)
        );
    }
}
