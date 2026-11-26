//! One tracing setup for every binary.
//!
//! Filter with `VICTORY_LOG` (falls back to `RUST_LOG`), e.g.
//! `VICTORY_LOG=victory_loader=trace,info`. Debug and trace output exist
//! only in builds with the `diagnostics` feature; a plain release build
//! logs info and above whatever the filter asks for.
//!
//! Besides the console, the session log ([`crate::session`]) sees every
//! event at info and above once a run starts it, whatever `VICTORY_LOG`
//! says. Fields named `spark.*` are for it alone: the console does not
//! show them, so its lines read as they always did.

use std::fmt;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::field::{MakeVisitor, Visit, VisitFmt, VisitOutput};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::format::{DefaultFields, Writer};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::session;

/// The most verbose level this build can log.
const CAP: LevelFilter = if cfg!(feature = "diagnostics") {
    LevelFilter::TRACE
} else {
    LevelFilter::INFO
};

pub fn init(default_level: &str) {
    let filter = std::env::var("VICTORY_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .ok()
        .and_then(|v| EnvFilter::try_new(v).ok())
        .unwrap_or_else(|| EnvFilter::new(default_level));
    let session_level = match session::layer_level() {
        session::Level::Error => LevelFilter::ERROR,
        session::Level::Warn => LevelFilter::WARN,
        session::Level::Info => LevelFilter::INFO,
        session::Level::Debug => LevelFilter::DEBUG,
        session::Level::Trace => LevelFilter::TRACE,
    };
    let _ = tracing_subscriber::registry()
        .with(CAP)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .fmt_fields(ConsoleFields::default())
                .with_writer(std::io::stderr)
                .with_filter(filter),
        )
        .with(session::SessionLayer.with_filter(session_level))
        .try_init();
}

/// Whether a field is for the session log alone.
fn hidden(field: &tracing::field::Field) -> bool {
    field.name().starts_with("spark.")
}

/// The console's usual fields, without the `spark.*` ones.
#[derive(Default)]
pub struct ConsoleFields(DefaultFields);

impl<'a> MakeVisitor<Writer<'a>> for ConsoleFields {
    type Visitor = Hide<<DefaultFields as MakeVisitor<Writer<'a>>>::Visitor>;

    fn make_visitor(&self, target: Writer<'a>) -> Self::Visitor {
        Hide(self.0.make_visitor(target))
    }
}

/// Passes every field but the hidden ones to the visitor it wraps.
pub struct Hide<V>(V);

impl<V: Visit> Visit for Hide<V> {
    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        if !hidden(field) {
            self.0.record_f64(field, value);
        }
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        if !hidden(field) {
            self.0.record_i64(field, value);
        }
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if !hidden(field) {
            self.0.record_u64(field, value);
        }
    }
    fn record_i128(&mut self, field: &tracing::field::Field, value: i128) {
        if !hidden(field) {
            self.0.record_i128(field, value);
        }
    }
    fn record_u128(&mut self, field: &tracing::field::Field, value: u128) {
        if !hidden(field) {
            self.0.record_u128(field, value);
        }
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if !hidden(field) {
            self.0.record_bool(field, value);
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if !hidden(field) {
            self.0.record_str(field, value);
        }
    }
    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        if !hidden(field) {
            self.0.record_error(field, value);
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if !hidden(field) {
            self.0.record_debug(field, value);
        }
    }
}

impl<V: VisitOutput<fmt::Result>> VisitOutput<fmt::Result> for Hide<V> {
    fn finish(self) -> fmt::Result {
        self.0.finish()
    }
}

impl<V: VisitFmt> VisitFmt for Hide<V> {
    fn writer(&mut self) -> &mut dyn fmt::Write {
        self.0.writer()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::layer::SubscriberExt;

    /// The console's text for one event, with `ConsoleFields` or without.
    fn console(hide: bool, emit: impl FnOnce()) -> String {
        #[derive(Clone, Default)]
        struct Buf(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buf = Buf::default();
        let out = buf.clone();
        let layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .without_time()
            .with_writer(move || out.clone());
        if hide {
            let s = tracing_subscriber::registry()
                .with(layer.fmt_fields(super::ConsoleFields::default()));
            tracing::subscriber::with_default(s, emit);
        } else {
            let s = tracing_subscriber::registry().with(layer);
            tracing::subscriber::with_default(s, emit);
        }
        String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn spark_fields_are_not_on_the_console() {
        let with = console(true, || {
            tracing::info!(
                spark.kind = "frame_rate",
                spark.fps = 30.0,
                ok = true,
                "30.0 fps"
            );
        });
        let plain = console(false, || tracing::info!(ok = true, "30.0 fps"));
        assert_eq!(with, plain);
        assert!(with.contains("30.0 fps ok=true"), "{with}");
        assert!(!with.contains("spark"));
    }
}
