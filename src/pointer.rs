//! JSON Pointer (RFC 6901) for addressing schema fragments.
//!
//! A JSON Pointer is a sequence of unescaped reference tokens, separated by
//! `/`, where `~0` decodes to `~` and `~1` decodes to `/`. The empty pointer
//! (the empty string) refers to the entire document.

use std::fmt;

use percent_encoding::percent_decode_str;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Token(pub String);

impl Token {
    pub fn decode(raw: &str) -> Self {
        let mut s = String::with_capacity(raw.len());
        let mut chars = raw.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '~'
                && let Some(&next) = chars.peek()
            {
                match next {
                    '0' => {
                        s.push('~');
                        chars.next();
                    }
                    '1' => {
                        s.push('/');
                        chars.next();
                    }
                    _ => s.push(c),
                }
            } else {
                s.push(c);
            }
        }
        Token(s)
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Pointer {
    pub tokens: Vec<Token>,
}

impl Pointer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn parse(s: &str) -> Option<Self> {
        if s == "#" {
            return Some(Self::new());
        }
        let rest = s.strip_prefix("#/")?;
        let rest = percent_decode_str(rest).decode_utf8().ok()?;
        let tokens = rest.split('/').map(Token::decode).collect::<Vec<_>>();
        Some(Self { tokens })
    }

    pub fn resolve<'a>(&self, root: &'a serde_json::Value) -> Option<&'a serde_json::Value> {
        let mut current = root;
        for token in &self.tokens {
            current = match current {
                serde_json::Value::Object(object) => object.get(&token.0)?,
                serde_json::Value::Array(array) => {
                    let index = token.0.parse::<usize>().ok()?;
                    array.get(index)?
                }
                _ => return None,
            };
        }
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_escape() {
        assert_eq!(Token::decode("foo").0, "foo");
        assert_eq!(Token::decode("~0").0, "~");
        assert_eq!(Token::decode("~1").0, "/");
        assert_eq!(Token::decode("a~1b~0c").0, "a/b~c");
    }

    #[test]
    fn parse_empty() {
        let p = Pointer::parse("#").unwrap();
        assert!(p.tokens.is_empty());
    }

    #[test]
    fn parse_with_escape() {
        let p = Pointer::parse("#/foo~1bar/baz~0qux").unwrap();
        assert_eq!(p.tokens[0].0, "foo/bar");
        assert_eq!(p.tokens[1].0, "baz~qux");
    }

    #[test]
    fn resolve_escaped() {
        let v = serde_json::json!({"foo/bar": {"baz~qux": 1}});
        let p = Pointer::parse("#/foo~1bar/baz~0qux").unwrap();
        assert_eq!(p.resolve(&v).unwrap(), 1);
    }
}
