use std::cell::RefCell;
use std::collections::HashSet;

thread_local! {
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static VERBOSE: RefCell<bool> = const { RefCell::new(false) };
    static WARNED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Enable or disable `notice:` output (default off).
pub fn set_verbose(enabled: bool) {
    VERBOSE.with(|v| *v.borrow_mut() = enabled);
}

pub fn is_verbose() -> bool {
    VERBOSE.with(|v| *v.borrow())
}

fn emit(level: &str, message: &str) {
    let line = format!("{level}: {message}");
    eprintln!("{line}");
    #[cfg(debug_assertions)]
    LOG.with(|log| log.borrow_mut().push(line));
}

/// Operational detail written to stderr when verbose mode is on.
pub fn notice(message: impl AsRef<str>) {
    if !is_verbose() {
        return;
    }
    emit("notice", message.as_ref());
}

/// Non-fatal issue written to stderr (always shown).
pub fn warn(message: impl AsRef<str>) {
    emit("warning", message.as_ref());
}

/// Like [`warn`], but emits at most once per message string in this thread.
pub fn warn_once(message: impl AsRef<str>) {
    let message = message.as_ref().to_string();
    let first = WARNED.with(|seen| seen.borrow_mut().insert(message.clone()));
    if first {
        emit("warning", &message);
    }
}

/// Clear deduplicated warning keys in debug builds (for tests).
pub fn clear_warned() {
    WARNED.with(|seen| seen.borrow_mut().clear());
}

/// Drain messages captured in debug builds (for tests).
pub fn take_log() -> Vec<String> {
    #[cfg(debug_assertions)]
    {
        LOG.with(|log| std::mem::take(&mut *log.borrow_mut()))
    }
    #[cfg(not(debug_assertions))]
    Vec::new()
}

/// Clear captured messages in debug builds (for tests).
pub fn clear_log() {
    #[cfg(debug_assertions)]
    LOG.with(|log| log.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notice_gated_behind_verbose() {
        clear_log();
        set_verbose(false);
        notice("hidden");
        assert!(take_log().is_empty());

        set_verbose(true);
        notice("visible");
        assert!(take_log().iter().any(|l| l.contains("visible")));
        set_verbose(false);
    }

    #[test]
    fn warn_always_emits() {
        clear_log();
        set_verbose(false);
        warn("always");
        assert!(take_log().iter().any(|l| l.contains("warning: always")));
    }

    #[test]
    fn notice_and_warn_append_to_test_log_when_verbose() {
        clear_log();
        set_verbose(true);
        notice("alpha");
        warn("beta");
        let log = take_log();
        assert_eq!(log.len(), 2);
        assert!(log[0].contains("notice: alpha"));
        assert!(log[1].contains("warning: beta"));
        set_verbose(false);
    }
}
