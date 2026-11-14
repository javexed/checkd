//! How a command line is started: directly, or through `/bin/sh`.
//!
//! The reference starts a plain command itself and hands anything it does
//! not want to split to the shell. The two differ where the program is
//! missing (the shell answers with code 127, a direct start with the
//! system's error) and in a few characters, so the choice is copied:
//!
//! - outside quotes, any of `& | * ? ~ ( ) $` or a backtick sends the
//!   line to the shell, and so does a `;`;
//! - so does a `=` in any word after the program;
//! - so does a backslash, `$` or backtick inside double quotes, and a
//!   quote that is not closed;
//! - everything else is split here. Words are divided by blanks. Single
//!   and double quotes keep their content as it is. A backslash outside
//!   quotes keeps the character after it;
//! - `NAME=value` words before the program are put into its environment;
//! - `<` and `>` are not special when the line is split here: they reach
//!   the program as arguments. A redirection works only when something
//!   else on the line sends it to the shell.

/// How to start a command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Start {
    /// Start `argv[0]` with these arguments and these additions to the
    /// environment.
    Direct {
        env: Vec<(String, String)>,
        argv: Vec<String>,
    },
    /// Give the whole line to `/bin/sh -c`.
    Shell,
}

/// Decides how a command line is started.
pub fn plan(line: &str) -> Start {
    let mut words: Vec<(String, bool)> = Vec::new(); // text, had a plain `=`
    let mut word: Option<(String, bool)> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if let Some(done) = word.take() {
                    words.push(done);
                }
            }
            '&' | '|' | '*' | '?' | '~' | '(' | ')' | '$' | '`' | ';' | '\n' => return Start::Shell,
            '\'' => {
                let (text, _) = word.get_or_insert_with(Default::default);
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => text.push(c),
                        None => return Start::Shell,
                    }
                }
            }
            '"' => {
                let (text, _) = word.get_or_insert_with(Default::default);
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\' | '$' | '`') | None => return Start::Shell,
                        Some(c) => text.push(c),
                    }
                }
            }
            '\\' => {
                let (text, _) = word.get_or_insert_with(Default::default);
                match chars.next() {
                    Some(c) => text.push(c),
                    None => return Start::Shell,
                }
            }
            c => {
                let (text, equals) = word.get_or_insert_with(Default::default);
                *equals |= c == '=';
                text.push(c);
            }
        }
    }
    words.extend(word);

    let mut env = Vec::new();
    let mut argv = Vec::new();
    for (text, equals) in words {
        if argv.is_empty() {
            if let Some((name, value)) = text.split_once('=').filter(|(name, _)| is_name(name)) {
                if equals {
                    env.push((name.to_string(), value.to_string()));
                    continue;
                }
            }
        } else if equals {
            return Start::Shell;
        }
        argv.push(text);
    }
    if argv.is_empty() {
        return Start::Shell;
    }
    Start::Direct { env, argv }
}

fn is_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct(line: &str) -> Vec<String> {
        match plan(line) {
            Start::Direct { argv, .. } => argv[1..].to_vec(),
            Start::Shell => panic!("{line} goes to the shell"),
        }
    }

    // Each line was run through the reference with a program that records
    // its arguments, or with one that does not exist to see who reports it.
    #[test]
    fn split_here() {
        assert_eq!(direct("p a > /dev/null"), ["a", ">", "/dev/null"]);
        assert_eq!(direct("p a < /dev/null"), ["a", "<", "/dev/null"]);
        assert_eq!(direct("p a >b 2>c"), ["a", ">b", "2>c"]);
        assert_eq!(
            direct(r#"p 'a b' "c d" e\ f g\\h"#),
            ["a b", "c d", "e f", r"g\h"]
        );
        assert_eq!(
            direct("p [a] {b,c} #d ^e %f"),
            ["[a]", "{b,c}", "#d", "^e", "%f"]
        );
        assert_eq!(
            direct(r#"p   "a 'b' c" 'd "e" f'"#),
            ["a 'b' c", r#"d "e" f"#]
        );
        assert_eq!(direct("p a   b\t\tc  "), ["a", "b", "c"]);
        assert_eq!(direct(r"p a\tb a\nb"), ["atb", "anb"]);
        assert_eq!(direct(r#"p x "" '' y"#), ["x", "", "", "y"]);
        assert_eq!(direct(r#"p a"b c"d"#), ["ab cd"]);
        assert_eq!(direct("p a'b c'd"), ["ab cd"]);
        assert_eq!(direct(r"p \a \' x"), ["a", "'", "x"]);
        assert_eq!(direct("p a # b"), ["a", "#", "b"]);
        assert_eq!(direct(r"p a\>b"), ["a>b"]);
        assert_eq!(
            direct("p -w 5 -c 10 --opt:val -H 192.0.2.1"),
            ["-w", "5", "-c", "10", "--opt:val", "-H", "192.0.2.1"]
        );
        assert_eq!(direct(r"p 'a'\''b'"), ["a'b"]);
        assert_eq!(direct(r#"p 'x' \"b\" c"#), ["x", "\"b\"", "c"]);
        assert_eq!(direct("p 'a*'"), ["a*"]);
        assert_eq!(direct(r#"p "a|b""#), ["a|b"]);
        assert_eq!(direct("p a!b"), ["a!b"]);
    }

    #[test]
    fn to_the_shell() {
        for line in [
            "p 2>&1",
            "p a|b",
            "p a | b",
            "p a&&b",
            "p a && b",
            "p a &",
            "p a*",
            "p a?",
            "p ~",
            "p (a)",
            "p $HOME",
            "p `a`",
            "p a=b",
            r#"p "a\"b""#,
            "p 'unterminated x",
            "p \"unterminated y",
            "p 'x' 'y' | cat",
        ] {
            assert_eq!(plan(line), Start::Shell, "{line}");
        }
    }

    #[test]
    fn environment_before_the_program() {
        assert_eq!(
            plan("X=1 Y='two words' /p e1 0 -"),
            Start::Direct {
                env: vec![
                    ("X".to_string(), "1".to_string()),
                    ("Y".to_string(), "two words".to_string())
                ],
                argv: vec![
                    "/p".to_string(),
                    "e1".to_string(),
                    "0".to_string(),
                    "-".to_string()
                ],
            }
        );
    }
}

/// What stands between the words of one of checkd's own command lines
/// once the engine has filled it in: a character no word can have.
pub const BETWEEN: char = '\u{1f}';

/// Whether a command line is one of checkd's own (`checkd:...`), which
/// starts no process and is read by no shell.
pub fn own(line: &str) -> bool {
    line.trim_start().starts_with("checkd:")
}

/// The words of one of checkd's own command lines. As the engine gives
/// it, they are apart already; as a person types it, quotes keep a word
/// with spaces together.
pub fn own_words(line: &str) -> Vec<String> {
    if line.contains(BETWEEN) { line.split(BETWEEN).map(str::to_string).collect() } else { words(line.trim_start()) }
}

/// The words of a line; single or double quotes keep a word with spaces
/// together, and there is no escaping.
pub fn words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut begun = false;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                begun = true;
            }
            None if c.is_whitespace() => {
                if begun || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                    begun = false;
                }
            }
            None => word.push(c),
        }
    }
    if begun || !word.is_empty() {
        words.push(word);
    }
    words
}
