use std::borrow::Cow;

/// Preserve literal braces so an input containing "{id}" has its own ranking key.
pub(super) fn message_pattern(message: &str) -> Cow<'_, str> {
    let mut output = String::new();
    let mut copied = 0;
    let mut chars = message.char_indices().peekable();
    let mut previous_label_end = None;
    while let Some((start, ch)) = chars.next() {
        let (end, replacement) = if word_char(ch) {
            while chars.next_if(|(_, ch)| word_char(*ch)).is_some() {}
            let end = chars.peek().map_or(message.len(), |(end, _)| *end);
            let word = &message[start..end];
            let labelled = previous_label_end.is_some_and(|prior| {
                message[prior..start]
                    .bytes()
                    .all(|b| matches!(b, b' ' | b':' | b'=' | b'#'))
            });
            previous_label_end = is_id_label(word).then_some(end);
            let replacement = if labelled && word.bytes().all(|byte| byte.is_ascii_digit()) {
                Some("{id}")
            } else if is_uuid(word) {
                Some("{uuid}")
            } else {
                None
            };
            (end, replacement)
        } else {
            let replacement = match ch {
                '{' => Some("{{"),
                '}' => Some("}}"),
                _ => None,
            };
            (start + ch.len_utf8(), replacement)
        };
        if let Some(replacement) = replacement {
            output.push_str(&message[copied..start]);
            output.push_str(replacement);
            copied = end;
        }
    }
    if copied == 0 {
        Cow::Borrowed(message)
    } else {
        output.push_str(&message[copied..]);
        Cow::Owned(output)
    }
}

fn word_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/')
}

fn is_id_label(word: &str) -> bool {
    [
        "id",
        "request",
        "request_id",
        "request-id",
        "job",
        "task",
        "pid",
    ]
    .iter()
    .any(|label| word.eq_ignore_ascii_case(label))
}

fn is_uuid(word: &str) -> bool {
    word.len() == 36
        && word.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_labelled_ids_and_standalone_uuids_only() {
        let message = "REQUEST: #184 failed; pid=44, trace 61F0D7A2-6FF9-4e05-9a37-572e1b4228e7";
        assert_eq!(
            message_pattern(message),
            "REQUEST: #{id} failed; pid={id}, trace {uuid}"
        );
        for literal in [
            "status 404",
            "IPv4 192.0.2.1",
            "request 1.2",
            "pid /123",
            "request 9ms",
            "überrequest 1",
        ] {
            assert_eq!(message_pattern(literal), literal);
        }
        assert_eq!(message_pattern("request {id}"), "request {{id}}");
        assert_ne!(
            message_pattern("request {id}"),
            message_pattern("request 12")
        );
        assert_eq!(message_pattern("ok 🦀 job 12"), "ok 🦀 job {id}");
    }
}
