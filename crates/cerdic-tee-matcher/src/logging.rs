//! Structured, colored logging in the shape reth uses for its node logs:
//! a dimmed RFC3339 timestamp, a fixed-width colored level, a dimmed
//! target, the message, then any structured fields as dimmed `key=value`
//! pairs. Readable in a terminal, greppable in a file (colors are dropped
//! automatically when stdout isn't a TTY, e.g. under `docker logs` piped
//! to a file).
//!
//! Level -> color mapping matches the convention most Rust node software
//! (reth included) has converged on: TRACE=magenta, DEBUG=blue, INFO=green,
//! WARN=yellow, ERROR=red. Anything reaching WARN or above is also bolded,
//! since those are the lines an operator scanning a scrollback actually
//! needs to catch.

use nu_ansi_term::{Color, Style};
use std::fmt;
use time::format_description::well_known::Rfc3339;
use tracing::{
    field::{Field, Visit},
    Event, Level, Subscriber,
};
use tracing_subscriber::{
    fmt::{format::Writer, FmtContext, FormatEvent, FormatFields},
    registry::LookupSpan,
    EnvFilter,
};

/// Reads `CERDIC_LOG` (falling back to `RUST_LOG`, then `info`) and installs
/// the formatter below as the process-wide default subscriber. Call once,
/// at the top of `main`, before anything else logs.
pub fn init() {
    let filter = EnvFilter::try_from_env("CERDIC_LOG")
        .or_else(|_| EnvFilter::try_from_env("RUST_LOG"))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .event_format(RethStyle)
        .init();
}

fn level_color(level: &Level) -> Style {
    let color = match *level {
        Level::TRACE => Color::Magenta,
        Level::DEBUG => Color::Blue,
        Level::INFO => Color::Green,
        Level::WARN => Color::Yellow,
        Level::ERROR => Color::Red,
    };
    let style = Style::new().fg(color);
    if matches!(*level, Level::WARN | Level::ERROR) {
        style.bold()
    } else {
        style
    }
}

/// Collects an event's fields into `message` (the `message` field, if
/// present) plus an ordered `key=value` tail for everything else. Mirrors
/// how reth's own formatter separates the human sentence from the
/// structured context that follows it.
#[derive(Default)]
struct FieldCollector {
    message: Option<String>,
    rest: Vec<(&'static str, String)>,
}

impl Visit for FieldCollector {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        } else {
            self.rest.push((field.name(), format!("{value:?}")));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.rest.push((field.name(), value.to_string()));
        }
    }
}

struct RethStyle;

impl<S, N> FormatEvent<S, N> for RethStyle
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let ansi = writer.has_ansi_escapes();
        let dim = if ansi { Style::new().dimmed() } else { Style::new() };

        // Timestamp, dimmed, subsecond RFC3339, UTC (the only sane choice
        // for a process whose logs get aggregated across enclaves/clouds).
        let now = time::OffsetDateTime::now_utc();
        let ts = now.format(&Rfc3339).unwrap_or_default();
        write!(writer, "{}", dim.paint(ts))?;
        write!(writer, " ")?;

        // Level, fixed 5-char width so every line's message column lines
        // up, colored per level_color above.
        let level = event.metadata().level();
        let style = if ansi { level_color(level) } else { Style::new() };
        write!(writer, "{}", style.paint(format!("{level:>5}")))?;
        write!(writer, " ")?;

        // Target, dimmed, e.g. `cerdic_tee_matcher::book`. This is the
        // single most useful field for filtering scrollback by subsystem,
        // so it stays even at the default (non-verbose) format.
        let target = event.metadata().target();
        write!(writer, "{}", dim.paint(target))?;
        write!(writer, ": ")?;

        // Span context, if any. reth shows the active span chain inline
        // so a log line from inside `settle_match{match_id=..}` is
        // self-describing without cross-referencing an earlier line.
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                write!(writer, "{}", dim.paint(span.name()))?;
                let ext = span.extensions();
                if let Some(fields) = ext.get::<tracing_subscriber::fmt::FormattedFields<N>>() {
                    if !fields.is_empty() {
                        write!(writer, "{}", dim.paint(format!("{{{fields}}}")))?;
                    }
                }
                write!(writer, "{}", dim.paint(":"))?;
            }
            write!(writer, " ")?;
        }

        // Message + trailing key=value fields.
        let mut collector = FieldCollector::default();
        event.record(&mut collector);

        if let Some(message) = &collector.message {
            write!(writer, "{message}")?;
        }

        for (key, value) in &collector.rest {
            write!(writer, " ")?;
            let key_style = if ansi { Style::new().fg(Color::Cyan) } else { Style::new() };
            write!(writer, "{}", key_style.paint(*key))?;
            write!(writer, "{}", dim.paint("="))?;
            write!(writer, "{value}")?;
        }

        writeln!(writer)
    }
}
