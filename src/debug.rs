// Only called from the debug_log! macro, which compiles to a no-op in release builds.
#[cfg(debug_assertions)]
pub fn enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();

    *ENABLED.get_or_init(|| {
        std::env::var_os("FROST_DEBUG")
            .map(|value| value != "0")
            .unwrap_or(false)
    })
}

#[cfg(any(debug_assertions, test))]
pub fn format_bytes(bytes: &[u8]) -> String {
    const MAX_BYTES: usize = 96;

    let mut out = String::new();
    let preview = if bytes.len() > MAX_BYTES {
        &bytes[..MAX_BYTES]
    } else {
        bytes
    };

    for &byte in preview {
        match byte {
            b'\x1b' => out.push_str("<ESC>"),
            b'\r' => out.push_str("<CR>"),
            b'\n' => out.push_str("<LF>"),
            b'\t' => out.push_str("<TAB>"),
            0x20..=0x7e => out.push(byte as char),
            _ => out.push_str(&format!("<0x{byte:02x}>")),
        }
    }

    if bytes.len() > MAX_BYTES {
        out.push_str(&format!("...(+{} bytes)", bytes.len() - MAX_BYTES));
    }

    out
}

#[macro_export]
macro_rules! debug_log {
    ($($arg:tt)*) => {
        #[cfg(debug_assertions)]
        {
            if $crate::debug::enabled() {
                eprintln!($($arg)*);
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::format_bytes;

    #[test]
    fn control_bytes_are_named_not_echoed() {
        assert_eq!(
            format_bytes(b"\x1b[31mhi\r\n\t\x07"),
            "<ESC>[31mhi<CR><LF><TAB><0x07>"
        );
    }

    #[test]
    fn long_payloads_are_truncated_with_remainder() {
        let bytes = vec![b'A'; 100];
        let rendered = format_bytes(&bytes);
        assert!(rendered.starts_with(&"A".repeat(96)));
        assert!(rendered.ends_with("...(+4 bytes)"));
        assert!(!rendered.contains(&*"A".repeat(97)));
    }
}
