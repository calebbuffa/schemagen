//! Diagnostic reporting during schema loading and generation.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub file: PathBuf,
    pub pointer: Option<String>,
}

impl Location {
    pub fn new(file: impl Into<PathBuf>) -> Self {
        Self {
            file: file.into(),
            pointer: None,
        }
    }
    pub fn with_pointer(mut self, pointer: impl Into<String>) -> Self {
        self.pointer = Some(pointer.into());
        self
    }
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.file.display())?;
        if let Some(p) = &self.pointer {
            write!(f, "{}", p)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub level: Level,
    pub location: Location,
    pub message: String,
}

impl Diagnostic {
    pub fn warning(location: Location, message: impl Into<String>) -> Self {
        Self {
            level: Level::Warning,
            location,
            message: message.into(),
        }
    }
    pub fn error(location: Location, message: impl Into<String>) -> Self {
        Self {
            level: Level::Error,
            location,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.level {
            Level::Warning => write!(f, "warning: {}: {}", self.location, self.message),
            Level::Error => write!(f, "error: {}: {}", self.location, self.message),
        }
    }
}

#[derive(Debug, Default)]
pub struct Sink {
    diagnostics: Vec<Diagnostic>,
    allow_lossy: bool,
}

impl Sink {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn allow_lossy(&mut self, lossy: bool) {
        self.allow_lossy = lossy;
    }
    pub fn report(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }
    pub fn warn(&mut self, location: Location, message: impl Into<String>) {
        self.report(Diagnostic::warning(location, message));
    }
    pub fn error(&mut self, location: Location, message: impl Into<String>) {
        self.report(Diagnostic::error(location, message));
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub fn has_errors(&self) -> bool {
        !self.allow_lossy && self.diagnostics.iter().any(|d| d.level == Level::Error)
    }
    pub fn has_warnings(&self) -> bool {
        self.diagnostics.iter().any(|d| d.level == Level::Warning)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sink_records_warnings() {
        let mut sink = Sink::new();
        sink.warn(Location::new("a.json"), "oops");
        assert!(sink.has_warnings());
        assert!(!sink.has_errors());
    }

    #[test]
    fn lossy_mode_suppresses_errors() {
        let mut sink = Sink::new();
        sink.allow_lossy(true);
        sink.error(Location::new("a.json"), "bad");
        assert!(!sink.has_errors());
    }
}
