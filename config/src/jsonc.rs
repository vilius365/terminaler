/// Strip JSONC comments (// line comments and /* block comments */) while respecting string literals.
pub fn strip_jsonc_comments(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        match bytes[i] {
            // String literal — copy verbatim including escape sequences
            b'"' => {
                result.push('"');
                i += 1;
                while i < len {
                    if bytes[i] == b'\\' && i + 1 < len {
                        result.push(bytes[i] as char);
                        result.push(bytes[i + 1] as char);
                        i += 2;
                    } else if bytes[i] == b'"' {
                        result.push('"');
                        i += 1;
                        break;
                    } else {
                        result.push(bytes[i] as char);
                        i += 1;
                    }
                }
            }
            // Potential comment start
            b'/' if i + 1 < len => {
                if bytes[i + 1] == b'/' {
                    // Line comment — skip until newline
                    i += 2;
                    while i < len && bytes[i] != b'\n' {
                        i += 1;
                    }
                } else if bytes[i + 1] == b'*' {
                    // Block comment — skip until */
                    i += 2;
                    while i + 1 < len {
                        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    // Handle unterminated block comment (skip to end)
                    if i >= len {
                        break;
                    }
                } else {
                    result.push('/');
                    i += 1;
                }
            }
            _ => {
                result.push(bytes[i] as char);
                i += 1;
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_line_comments() {
        let input = r#"{"key": "value" // this is a comment
}"#;
        // Whitespace before the comment is preserved (JSON ignores it)
        let expected = "{\"key\": \"value\" \n}";
        assert_eq!(strip_jsonc_comments(input), expected);
    }

    #[test]
    fn test_strip_block_comments() {
        let input = r#"{"key": /* comment */ "value"}"#;
        let expected = r#"{"key":  "value"}"#;
        assert_eq!(strip_jsonc_comments(input), expected);
    }

    #[test]
    fn test_preserve_strings() {
        let input = r#"{"url": "http://example.com"}"#;
        assert_eq!(strip_jsonc_comments(input), input);
    }

    #[test]
    fn test_preserve_comment_in_string() {
        let input = r#"{"msg": "hello // world"}"#;
        assert_eq!(strip_jsonc_comments(input), input);
    }

    #[test]
    fn test_escaped_quote_in_string() {
        let input = r#"{"msg": "say \"hello\" // not a comment"}"#;
        assert_eq!(strip_jsonc_comments(input), input);
    }

    #[test]
    fn test_no_comments() {
        let input = r#"{"key": "value"}"#;
        assert_eq!(strip_jsonc_comments(input), input);
    }
}
