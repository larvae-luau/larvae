//! System level concerns: the larvae home directory layout

pub mod paths;

/*
A stand-in text for bytes that are not UTF-8.

Luau reads any byte inside a string literal, and larvae reads files as
UTF-8. A read-only pass does not write the bytes back, so it can analyze a
stand-in: every invalid byte becomes 0x1A, one for one, so every offset
stays true and every diagnostic points where it did. A pass that writes
output must refuse the file instead, because a splice of the stand-in
would write the stand-in bytes into it.
*/
pub fn utf8_stand_in(bytes: Vec<u8>) -> (String, usize) {
    let mut bytes = match String::from_utf8(bytes) {
        Ok(text) => return (text, 0),

        Err(e) => e.into_bytes(),
    };

    let mut replaced = 0usize;
    let mut at = 0usize;

    loop {
        match std::str::from_utf8(&bytes[at..]) {
            Ok(_) => break,

            Err(e) => {
                let valid = e.valid_up_to();
                let bad = e.error_len().unwrap_or(bytes.len() - at - valid);

                for b in &mut bytes[at + valid..at + valid + bad] {
                    *b = 0x1A;
                }

                replaced += bad;
                at += valid + bad;
            }
        }
    }

    let text = String::from_utf8(bytes).expect("every invalid byte was replaced");

    (text, replaced)
}

#[cfg(test)]
mod stand_in_tests {
    use super::utf8_stand_in;

    #[test]
    fn valid_text_passes_through_untouched() {
        let (text, n) = utf8_stand_in("local x = 1\n".into());

        assert_eq!(text, "local x = 1\n");
        assert_eq!(n, 0);
    }

    /// The length must hold, or every later offset would drift.
    #[test]
    fn invalid_bytes_swap_one_for_one() {
        let bytes = b"s = \"a\xE4b\xFF\xFE\"\n".to_vec();
        let len = bytes.len();
        let (text, n) = utf8_stand_in(bytes);

        assert_eq!(text.len(), len);
        assert_eq!(n, 3);
        assert_eq!(text, "s = \"a\u{1A}b\u{1A}\u{1A}\"\n");
    }

    /// A truncated sequence at the end of the file is replaced too.
    #[test]
    fn a_truncated_tail_is_replaced() {
        let (text, n) = utf8_stand_in(b"x\xE2\x82".to_vec());

        assert_eq!(text.len(), 3);
        assert!(n >= 1);
    }
}
