use nu_parser::{Token, lex};

/// Each pass rewrites one heredoc, so a script cannot keep the loop going forever.
const MAX_HEREDOCS: usize = 32;

/// Characters that make a heredoc's command line more than a simple command.
const SHELL_SYNTAX: &[char] = &[
    '|', ';', '&', '<', '>', '(', ')', '{', '}', '$', '`', '\'', '"', '\\',
];

/// Turns bash heredocs, which Nushell has no syntax for, into raw strings. `cat > F <<'X'`
/// becomes a whole-file write with `kraai-edit-file` when the shell has it, or `save -f`
/// otherwise, and `cmd <<'X'` pipes the text into `cmd`. Returns the script and one change
/// label for each heredoc it rewrote, or `None` when a heredoc has no safe rewrite, because the
/// other rules would then misread its body as Nushell.
pub(super) fn rewrite(source: &str, edit_file: bool) -> Option<(String, Vec<String>)> {
    let mut current = source.to_owned();
    let mut changes = Vec::new();
    for _ in 0..MAX_HEREDOCS {
        match next_heredoc(&current, edit_file) {
            None => return Some((current, changes)),
            Some(Found::Unsupported) => return None,
            Some(Found::Rewrite {
                start,
                end,
                replacement,
                change,
            }) => {
                current.replace_range(start..end, &replacement);
                changes.push(change);
            }
        }
    }
    None
}

enum Found {
    /// The heredoc from its command line through its terminator, and what replaces it.
    Rewrite {
        start: usize,
        end: usize,
        replacement: String,
        change: String,
    },
    Unsupported,
}

/// The first heredoc outside any Nushell string, if the script has one.
fn next_heredoc(source: &str, edit_file: bool) -> Option<Found> {
    let (tokens, _) = lex(source.as_bytes(), 0, &[], &[], false);
    let lines: Vec<(usize, &str)> = source
        .split_inclusive('\n')
        .scan(0, |offset, line| {
            let start = *offset;
            *offset += line.len();
            Some((start, line))
        })
        .collect();
    for (index, (line_start, line)) in lines.iter().enumerate() {
        let Some(heredoc) = Heredoc::parse(line) else {
            continue;
        };
        let operator = line_start.saturating_add(heredoc.operator_at);
        if in_string(source.as_bytes(), &tokens, *line_start, operator) {
            continue;
        }
        let rest = lines.get(index.saturating_add(1)..).unwrap_or_default();
        let Some(length) = rest
            .iter()
            .position(|(_, body)| without_newline(body) == heredoc.tag)
        else {
            return Some(Found::Unsupported);
        };
        let body: String = rest
            .get(..length)
            .unwrap_or_default()
            .iter()
            .map(|(_, line)| *line)
            .collect();
        let expands = !heredoc.quoted && body.contains(['$', '`', '\\']);
        let (Some(destination), Some((terminator_start, terminator)), false) =
            (heredoc.destination, rest.get(length), expands)
        else {
            return Some(Found::Unsupported);
        };
        let (text, change) = heredoc.replacement(destination, &body, edit_file);
        let newline = terminator.get(without_newline(terminator).len()..)?;
        return Some(Found::Rewrite {
            start: *line_start,
            end: terminator_start.saturating_add(terminator.len()),
            replacement: format!("{}{text}{newline}", heredoc.indent),
            change,
        });
    }
    None
}

/// Whether `operator` lies in a string that started before its line, looking inside blocks.
fn in_string(source: &[u8], tokens: &[Token], line_start: usize, operator: usize) -> bool {
    let Some(token) = tokens
        .iter()
        .find(|token| token.span.start <= operator && operator < token.span.end)
    else {
        return false;
    };
    if token.span.start >= line_start {
        return false;
    }
    let (start, end) = (token.span.start, token.span.end);
    match source.get(start) {
        Some(b'{' | b'(' | b'[') if end.saturating_sub(start) >= 2 => {
            let inner = start.saturating_add(1);
            let input = source.get(inner..end.saturating_sub(1)).unwrap_or_default();
            let (tokens, _) = lex(input, inner, &[], &[], false);
            in_string(source, &tokens, line_start, operator)
        }
        _ => true,
    }
}

struct Heredoc<'a> {
    indent: &'a str,
    /// The command line as written, such as `cat > src/lib.rs <<'RS'`.
    written: &'a str,
    /// Where `<<` starts in the line.
    operator_at: usize,
    tag: &'a str,
    quoted: bool,
    /// Where the text goes, or `None` for a heredoc this rewrite does not understand.
    destination: Option<Destination<'a>>,
}

#[derive(Clone, Copy)]
enum Destination<'a> {
    File(&'a str),
    Command(&'a str),
}

impl<'a> Heredoc<'a> {
    fn parse(line: &'a str) -> Option<Self> {
        let content = without_newline(line);
        let indent_length = content.len().saturating_sub(content.trim_start().len());
        let indent = content.get(..indent_length)?;
        let operator_at = content.find("<<")?;
        let after = content.get(operator_at.saturating_add(2)..)?;
        if after.contains("<<") || after.starts_with(['<', '-']) {
            return None;
        }
        let after = after.trim_start();
        let (tag, quoted, rest) = match after.chars().next()? {
            quote @ ('\'' | '"') => {
                let inner = after.get(1..)?;
                let close = inner.find(quote)?;
                (
                    inner.get(..close)?,
                    true,
                    inner.get(close.saturating_add(1)..)?,
                )
            }
            _ => {
                let length = after
                    .find(|character: char| !is_tag_character(character))
                    .unwrap_or(after.len());
                (after.get(..length)?, false, after.get(length..)?)
            }
        };
        if tag.is_empty() || !tag.chars().all(is_tag_character) {
            return None;
        }
        let command = content.get(indent_length..operator_at)?.trim_end();
        let words: Vec<&str> = command
            .split_whitespace()
            .chain(rest.split_whitespace())
            .collect();
        Some(Self {
            indent,
            written: content.trim(),
            operator_at,
            tag,
            quoted,
            destination: destination(command, rest.trim(), &words),
        })
    }

    fn replacement(
        &self,
        destination: Destination<'_>,
        body: &str,
        edit_file: bool,
    ) -> (String, String) {
        let raw = raw_string(body);
        let (text, form) = match destination {
            Destination::File(path) if edit_file => (
                format!("kraai-edit-file {path} --overwrite --contents {raw}"),
                format!("kraai-edit-file {path} --overwrite --contents r#'…'#"),
            ),
            Destination::File(path) => (
                format!("{raw} | save -f {path}"),
                format!("r#'…'# | save -f {path}"),
            ),
            Destination::Command(command) => {
                (format!("{raw} | {command}"), format!("r#'…'# | {command}"))
            }
        };
        (text, format!("`{}` → `{form}`", self.written))
    }
}

/// `cat > F` with the heredoc before or after the redirect writes a file; a plain command with
/// nothing after the heredoc reads it as input.
fn destination<'a>(command: &'a str, rest: &str, words: &[&'a str]) -> Option<Destination<'a>> {
    if let [head, rest @ ..] = words
        && matches!(*head, "cat" | "^cat")
    {
        let path = match rest {
            [operator, path] if matches!(*operator, ">" | "o>") => Some(*path),
            [redirect] => redirect.strip_prefix('>'),
            _ => None,
        };
        if let Some(path) = path {
            return is_plain_path(path).then_some(Destination::File(path));
        }
    }
    let simple = rest.is_empty()
        && !words.is_empty()
        && !words.iter().any(|word| word.contains(SHELL_SYNTAX));
    simple.then_some(Destination::Command(command))
}

fn is_plain_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('-')
        && path
            .chars()
            .all(|character| character.is_alphanumeric() || "/._-+@,:".contains(character))
}

fn is_tag_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn without_newline(line: &str) -> &str {
    line.strip_suffix('\n')
        .map_or(line, |line| line.strip_suffix('\r').unwrap_or(line))
}

/// A Nushell raw string holding `text` verbatim, with enough `#`s that `text` cannot close it.
/// The opening quote counts too: `r#'#x'#` closes right after its opening `'`.
fn raw_string(text: &str) -> String {
    let quoted = format!("'{text}");
    let mut hashes = String::from("#");
    while quoted.contains(&format!("'{hashes}")) {
        hashes.push('#');
    }
    format!("r{hashes}'{text}'{hashes}")
}
