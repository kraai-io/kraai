use nu_parser::{Token, TokenContents, lex};
use nu_protocol::engine::StateWorkingSet;
use nu_protocol::{ParseError, Span};

/// Built-in commands named like a common external program. A bash-style call to one of them
/// fails to parse, so it runs the external program instead.
const SHADOWED_PROGRAMS: [&str; 18] = [
    "cp", "date", "du", "echo", "find", "ls", "mkdir", "mktemp", "mv", "ps", "rm", "sort", "tee",
    "touch", "uname", "uniq", "which", "whoami",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Edit {
    start: usize,
    end: usize,
    text: String,
    /// What the edit changed, such as `` `2>` → `e>` ``.
    pub(super) change: String,
}

/// Collects the edits for one round. `offset` is where the parsed source starts in the working
/// set's span space.
pub(super) fn edits(working_set: &StateWorkingSet, source: &str, offset: usize) -> Vec<Edit> {
    let mut scan = Scan {
        working_set,
        source: source.as_bytes(),
        edits: Vec::new(),
        commands: Vec::new(),
    };
    scan.block(0, source.len());
    for error in &working_set.parse_errors {
        let span = error.span();
        if let (Some(start), Some(end)) =
            (span.start.checked_sub(offset), span.end.checked_sub(offset))
            && start <= end
            && end <= source.len()
        {
            scan.repair(error, start, end);
        }
    }
    let mut edits = scan.edits;
    edits.retain(|edit| source.is_char_boundary(edit.start) && source.is_char_boundary(edit.end));
    edits.sort_by_key(|edit| (edit.start, edit.end));
    edits.dedup();
    let mut covered = 0;
    edits.retain(|edit| {
        let keep = edit.start >= covered;
        if keep {
            covered = edit.end;
        }
        keep
    });
    edits
}

pub(super) fn apply(source: &str, edits: &[Edit]) -> String {
    let mut output = String::with_capacity(source.len().saturating_add(16));
    let mut cursor = 0;
    for edit in edits {
        output.push_str(source.get(cursor..edit.start).unwrap_or_default());
        output.push_str(&edit.text);
        cursor = edit.end;
    }
    output.push_str(source.get(cursor..).unwrap_or_default());
    output
}

struct Scan<'a> {
    working_set: &'a StateWorkingSet<'a>,
    source: &'a [u8],
    edits: Vec<Edit>,
    commands: Vec<Command>,
}

struct Command {
    span: Span,
    head: Span,
    /// The head and the next word name a subcommand, such as `date now`.
    subcommand: bool,
}

/// A bash `2>&1`, and the pipe after it if there is one.
#[derive(Clone, Copy)]
struct Merge {
    span: Span,
    pipe: Option<Span>,
}

impl Scan<'_> {
    fn bytes(&self, span: Span) -> &[u8] {
        self.source.get(span.start..span.end).unwrap_or_default()
    }

    fn edit(&mut self, start: usize, end: usize, text: impl Into<String>, change: String) {
        self.edits.push(Edit {
            start,
            end,
            text: text.into(),
            change,
        });
    }

    fn block(&mut self, start: usize, end: usize) {
        let Some(input) = self.source.get(start..end) else {
            return;
        };
        let (tokens, _) = lex(input, start, &[], &[], false);
        let mut tokens = tokens
            .iter()
            .filter(|token| token.contents != TokenContents::Comment)
            .peekable();
        let mut command = Vec::new();
        while let Some(token) = tokens.next() {
            match token.contents {
                TokenContents::Pipe if self.bytes(token.span) == b"2>&1" => {
                    let pipe = tokens
                        .peek()
                        .filter(|next| {
                            next.contents == TokenContents::Pipe && self.bytes(next.span) == b"|"
                        })
                        .map(|next| next.span);
                    self.command(
                        &command,
                        Some(Merge {
                            span: token.span,
                            pipe,
                        }),
                    );
                    command.clear();
                }
                TokenContents::Pipe
                | TokenContents::PipePipe
                | TokenContents::Semicolon
                | TokenContents::Eol
                | TokenContents::ErrGreaterPipe
                | TokenContents::OutErrGreaterPipe => {
                    self.command(&command, None);
                    command.clear();
                }
                _ => command.push(token),
            }
        }
        self.command(&command, None);
    }

    /// Scans the contents of `(...)`, `{...}`, and `[...]` items.
    fn nested(&mut self, span: Span) {
        let bytes = self.bytes(span);
        let (Some(open), Some(close)) = (bytes.first(), bytes.last()) else {
            return;
        };
        if bytes.len() < 2 {
            return;
        }
        let (start, end) = (span.start.saturating_add(1), span.end.saturating_sub(1));
        match (open, close) {
            (b'(', b')') => self.block(start, end),
            (b'{', b'}') => self.block(self.closure_body(start, end), end),
            (b'[', b']') => {
                let Some(input) = self.source.get(start..end) else {
                    return;
                };
                let (tokens, _) = lex(input, start, b",", &[], false);
                for token in &tokens {
                    if token.contents == TokenContents::Item {
                        self.nested(token.span);
                    }
                }
            }
            _ => {}
        }
    }

    /// Skips a closure's `|params|` so they are not read as pipes.
    fn closure_body(&self, start: usize, end: usize) -> usize {
        let inner = self.source.get(start..end).unwrap_or_default();
        let Some(open) = inner.iter().position(|byte| !byte.is_ascii_whitespace()) else {
            return start;
        };
        if inner.get(open) != Some(&b'|') {
            return start;
        }
        inner
            .iter()
            .skip(open.saturating_add(1))
            .position(|byte| *byte == b'|')
            .map_or(start, |close| {
                start
                    .saturating_add(open)
                    .saturating_add(close)
                    .saturating_add(2)
            })
    }

    fn command(&mut self, tokens: &[&Token], merge: Option<Merge>) {
        for token in tokens {
            if token.contents == TokenContents::Item {
                self.nested(token.span);
            }
        }
        let Some(head_index) = self.head_index(tokens) else {
            return;
        };
        let (Some(first), Some(head), Some(last)) =
            (tokens.first(), tokens.get(head_index), tokens.last())
        else {
            return;
        };
        let args = tokens
            .get(head_index.saturating_add(1)..)
            .unwrap_or_default();
        let head_bytes = self.bytes(head.span);
        let external = self.is_external(head_bytes);
        let subcommand = args.first().is_some_and(|next| {
            let mut name = head_bytes.to_vec();
            name.push(b' ');
            name.extend_from_slice(self.bytes(next.span));
            self.working_set.find_decl(&name).is_some()
        });
        self.commands.push(Command {
            span: Span::new(first.span.start, last.span.end),
            head: head.span,
            subcommand,
        });

        let merged = merge.is_some_and(|merge| merge.pipe.is_none());
        let mut redirects_out = false;
        for (index, token) in args.iter().enumerate() {
            let has_target = args
                .get(index.saturating_add(1))
                .is_some_and(|next| next.contents == TokenContents::Item);
            let bytes = self.bytes(token.span);
            let replacement = match token.contents {
                TokenContents::ErrGreaterThan if bytes == b"2>" => {
                    Some((String::from("e>"), change("2>", "e>")))
                }
                TokenContents::OutGreaterThan if merged => {
                    redirects_out = true;
                    Some((String::from("o+e>"), change("> … 2>&1", "o+e>")))
                }
                TokenContents::OutGreaterGreaterThan if merged => {
                    redirects_out = true;
                    Some((String::from("o+e>>"), change(">> … 2>&1", "o+e>>")))
                }
                TokenContents::Item => {
                    item_redirect(bytes, external, merged, has_target).map(|(text, out, change)| {
                        redirects_out |= out;
                        (text, change)
                    })
                }
                _ => None,
            };
            if let Some((text, change)) = replacement {
                self.edit(token.span.start, token.span.end, text, change);
            }
        }

        if let Some(merge) = merge {
            let (end, text, bash) = match merge.pipe {
                _ if !external || redirects_out => (merge.span.end, "", "2>&1"),
                Some(pipe) => (pipe.end, "o+e>|", "2>&1 |"),
                None => (merge.span.end, "o+e>| collect", "2>&1"),
            };
            let label = if text.is_empty() {
                String::from("removed `2>&1`")
            } else {
                change(bash, text)
            };
            self.edit(merge.span.start, end, text, label);
        }
    }

    /// The command's head, after `let x =` or `$x =` when the command is an assignment.
    fn head_index(&self, tokens: &[&Token]) -> Option<usize> {
        let first = self.bytes(tokens.first()?.span);
        let assigns = matches!(first, b"let" | b"mut" | b"const") || first.starts_with(b"$");
        let start = tokens
            .iter()
            .position(|token| token.contents == TokenContents::AssignmentOperator)
            .filter(|_| assigns)
            .map_or(0, |index| index.saturating_add(1));
        tokens
            .get(start)
            .filter(|token| token.contents == TokenContents::Item)
            .map(|_| start)
    }

    fn is_external(&self, head: &[u8]) -> bool {
        match head.first() {
            Some(b'^') => true,
            Some(byte)
                if byte.is_ascii_alphabetic() || matches!(byte, b'.' | b'/' | b'_' | b'~') =>
            {
                self.working_set.find_decl(head).is_none()
            }
            _ => false,
        }
    }

    /// Some errors cover only `$NAME/` of `$NAME/path`; the rewrite needs the whole word.
    fn word_end(&self, end: usize) -> usize {
        let rest = self.source.get(end..).unwrap_or_default();
        let length = rest
            .iter()
            .position(|byte| byte.is_ascii_whitespace() || b"|;)}]".contains(byte))
            .unwrap_or(rest.len());
        end.saturating_add(length)
    }

    fn repair(&mut self, error: &ParseError, start: usize, end: usize) {
        let env_error = match error {
            ParseError::EnvVarNotVar(..) | ParseError::VariableNotFound(..) => true,
            ParseError::ExpectedWithDidYouMean(expected, ..) => *expected == "valid variable name",
            _ => false,
        };
        if env_error {
            let end = self.word_end(end);
            if let Some((replacement, label)) = self.source.get(start..end).and_then(env_variable) {
                self.edit(start, end, replacement, label);
            }
            return;
        }
        let flag_owner = match error {
            ParseError::UnknownFlag(name, ..) => Some(name.as_bytes()),
            ParseError::ExtraPositional(..) => None,
            _ => return,
        };
        if cfg!(windows) {
            return;
        }
        let Some(command) = self
            .commands
            .iter()
            .filter(|command| command.span.start <= start && end <= command.span.end)
            .min_by_key(|command| command.span.end.saturating_sub(command.span.start))
        else {
            return;
        };
        let head = self.bytes(command.head);
        let shadowed = SHADOWED_PROGRAMS
            .iter()
            .any(|program| program.as_bytes() == head);
        if shadowed && !command.subcommand && flag_owner.is_none_or(|owner| owner == head) {
            let at = command.head.start;
            let name = String::from_utf8_lossy(head).into_owned();
            self.edit(at, at, "^", change(&name, &format!("^{name}")));
        }
    }
}

fn change(bash: &str, nushell: &str) -> String {
    format!("`{bash}` → `{nushell}`")
}

/// The Nushell form of a bash redirection argument, whether it redirects stdout, and its change
/// label.
fn item_redirect(
    bytes: &[u8],
    external: bool,
    merged: bool,
    has_target: bool,
) -> Option<(String, bool, String)> {
    let text = std::str::from_utf8(bytes).ok()?;
    for (bash, nushell) in [("2>>", "e>>"), ("2>", "e>")] {
        if let Some(target) = text.strip_prefix(bash) {
            return redirect(nushell, target, has_target)
                .map(|text| (text, false, change(bash, nushell)));
        }
    }
    if !external {
        return None;
    }
    let (append, write) = if merged {
        ((">> … 2>&1", "o+e>>"), ("> … 2>&1", "o+e>"))
    } else {
        ((">>", "o>>"), (">", "o>"))
    };
    let (operator, label, target) = [
        ("&>>", ("&>>", "o+e>>")),
        ("&>", ("&>", "o+e>")),
        (">>", append),
        (">", write),
    ]
    .into_iter()
    .find_map(|(bash, (label, nushell))| {
        text.strip_prefix(bash)
            .map(|target| (nushell, change(label, nushell), target))
    })?;
    redirect(operator, target, has_target).map(|text| (text, true, label))
}

fn redirect(operator: &str, target: &str, has_target: bool) -> Option<String> {
    if target.starts_with(['&', '=', '>', '|']) {
        return None;
    }
    if target.is_empty() {
        return has_target.then(|| operator.to_owned());
    }
    Some(format!("{operator} {target}"))
}

/// `$HOME` becomes `$env.HOME`, and `$HOME/path` becomes `$"($env.HOME)/path"`.
fn env_variable(bytes: &[u8]) -> Option<(String, String)> {
    let text = std::str::from_utf8(bytes).ok()?.strip_prefix('$')?;
    let (name, rest) = text.split_at(text.find('/').unwrap_or(text.len()));
    let is_env_name = name.bytes().any(|byte| byte.is_ascii_uppercase())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        && !name.starts_with(|c: char| c.is_ascii_digit());
    if !is_env_name {
        return None;
    }
    let label = change(&format!("${name}"), &format!("$env.{name}"));
    if rest.is_empty() {
        return Some((format!("$env.{name}"), label));
    }
    rest.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
        .then(|| (format!("$\"($env.{name}){rest}\""), label))
}
