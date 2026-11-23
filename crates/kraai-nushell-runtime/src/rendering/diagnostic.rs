use std::fmt::Write;
use std::path::Path;

use miette::{Severity, SourceCode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Diagnostic {
    warning: bool,
    message: String,
    causes: Vec<String>,
    sources: Vec<Source>,
    notes: Vec<String>,
    help: Option<String>,
    code: Option<String>,
    related: Vec<Diagnostic>,
}

pub(crate) struct Origin<'a> {
    pub(crate) script: &'a str,
    pub(crate) cwd: Option<&'a Path>,
}

impl Origin<'_> {
    fn name(&self, name: &str) -> Option<String> {
        if name == self.script {
            return None;
        }
        let relative = self
            .cwd
            .and_then(|cwd| Path::new(name).strip_prefix(cwd).ok());
        Some(relative.map_or_else(|| name.to_owned(), |path| path.display().to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Source {
    name: Option<String>,
    lines: Vec<(usize, String)>,
    labels: Vec<Label>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Label {
    start: Position,
    end: Position,
    text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct Position {
    line: usize,
    column: usize,
}

impl Diagnostic {
    pub(crate) fn new(
        diagnostic: &dyn miette::Diagnostic,
        fallback: &dyn SourceCode,
        default_code: Option<&str>,
        origin: &Origin<'_>,
    ) -> Self {
        let source = diagnostic.source_code().unwrap_or(fallback);
        let mut built = Self {
            warning: diagnostic.severity() == Some(Severity::Warning),
            message: diagnostic.to_string(),
            causes: causes(diagnostic),
            sources: Vec::new(),
            notes: Vec::new(),
            help: diagnostic.help().map(|help| help.to_string()),
            code: diagnostic
                .code()
                .map(|code| code.to_string())
                .or_else(|| default_code.map(str::to_owned)),
            related: diagnostic
                .related()
                .into_iter()
                .flatten()
                .map(|related| Self::new(related, source, None, origin))
                .collect(),
        };
        for label in diagnostic.labels().into_iter().flatten() {
            built.place(source, &label, origin);
        }
        built
    }

    fn place(&mut self, source: &dyn SourceCode, label: &miette::LabeledSpan, origin: &Origin<'_>) {
        let text = label.label().map(str::to_owned);
        let Ok(contents) = source.read_span(label.inner(), 1, 1) else {
            self.notes.extend(text);
            return;
        };
        let data = contents.data();
        let offset = label.offset().saturating_sub(contents.span().offset());
        let start = position(data, contents.line(), offset);
        let end = position(
            data,
            contents.line(),
            offset + label.len().saturating_sub(1),
        );
        let name = contents.name().and_then(|name| origin.name(name));
        let index = match self.sources.iter().position(|source| source.name == name) {
            Some(index) => index,
            None => {
                self.sources.push(Source {
                    name,
                    lines: Vec::new(),
                    labels: Vec::new(),
                });
                self.sources.len() - 1
            }
        };
        let Some(source) = self.sources.get_mut(index) else {
            return;
        };
        for line in [start.line, end.line] {
            if !source.lines.iter().any(|(known, _)| *known == line) {
                let text = String::from_utf8_lossy(
                    data.split(|&byte| byte == b'\n')
                        .nth(line - contents.line())
                        .unwrap_or_default(),
                )
                .trim_end_matches('\r')
                .to_owned();
                source.lines.push((line, text));
            }
        }
        source.labels.push(Label { start, end, text });
    }

    pub(crate) fn render(&self) -> String {
        let mut out = String::new();
        self.render_into(&mut out);
        out.pop();
        out
    }

    fn render_into(&self, out: &mut String) {
        let kind = if self.warning { "Warning" } else { "Error" };
        let _ = writeln!(out, "{kind}: {}", self.message);
        for cause in &self.causes {
            let _ = writeln!(out, "caused by: {cause}");
        }
        for source in &self.sources {
            source.render(out);
        }
        for note in &self.notes {
            let _ = writeln!(out, "  {note}");
        }
        if let Some(help) = &self.help {
            let _ = writeln!(out, "help: {help}");
        }
        if let Some(code) = &self.code {
            let _ = writeln!(out, "code: {code}");
        }
        for related in &self.related {
            related.render_into(out);
        }
    }
}

impl Source {
    fn render(&self, out: &mut String) {
        let mut lines: Vec<_> = self.lines.iter().collect();
        lines.sort_by_key(|(line, _)| *line);
        let mut labels: Vec<_> = self.labels.iter().collect();
        labels.sort_by_key(|label| label.start);
        for (line, text) in lines {
            let _ = write!(out, "line {}", line + 1);
            if let Some(name) = &self.name {
                let _ = write!(out, " of {name}");
            }
            let _ = writeln!(out, ": {text}");
            for label in &labels {
                if label.start.line == *line {
                    out.push_str("  ");
                    if label.end.line != *line {
                        let _ = write!(out, "starting at column {}", label.start.column);
                    } else if label.end.column > label.start.column {
                        let _ = write!(
                            out,
                            "at columns {} to {}",
                            label.start.column, label.end.column
                        );
                    } else {
                        let _ = write!(out, "at column {}", label.start.column);
                    }
                    if let Some(text) = &label.text {
                        let _ = write!(out, ": {text}");
                    }
                    out.push('\n');
                } else if label.end.line == *line {
                    let _ = writeln!(out, "  ending at column {}", label.end.column);
                }
            }
        }
    }
}

fn causes(diagnostic: &dyn miette::Diagnostic) -> Vec<String> {
    let mut causes = Vec::new();
    let mut next = diagnostic.diagnostic_source();
    let mut error = next.is_none().then(|| diagnostic.source()).flatten();
    loop {
        if let Some(cause) = next {
            causes.push(cause.to_string());
            next = cause.diagnostic_source();
            error = next.is_none().then(|| cause.source()).flatten();
        } else if let Some(cause) = error {
            causes.push(cause.to_string());
            error = cause.source();
        } else {
            return causes;
        }
    }
}

fn position(data: &[u8], first_line: usize, offset: usize) -> Position {
    let before = data.get(..offset).unwrap_or(data);
    let line_start = before
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |index| index + 1);
    Position {
        line: first_line + before.iter().filter(|&&byte| byte == b'\n').count(),
        column: String::from_utf8_lossy(before.get(line_start..).unwrap_or_default())
            .chars()
            .count()
            + 1,
    }
}

#[cfg(test)]
mod tests;
