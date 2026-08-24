#![cfg(unix)]

//! Regression coverage for display-line based scrollback accounting.
//!
//! `scroll_buffer_size` bounds the scrollback in *display* lines. A line wider
//! than the viewport wraps into several display lines, so writing many wide
//! lines must not let the scroll indicator's total exceed the configured limit.
//! Before the display-line fix the total drifted well past it (e.g. `SCROLL 0/121`
//! with a limit of 100), because eviction counted logical rows while the
//! indicator counted display lines.

use zellij_integration_tests::{
    claim_first_terminal_and_wait_for_prompt, keys, TestRunner, TestSession, TERMINAL_SIZE,
};

const SCROLL_BUFFER_SIZE: usize = 100;

const LAST_LINE_MARKER: &str = "done-filling";

/// Parse the total out of the `SCROLL <pos>/<total>` indicator.
fn scroll_total(rendered: &str) -> Option<usize> {
    let idx = rendered.find("SCROLL ")?;
    let rest = &rendered[idx + "SCROLL ".len()..];
    let slash = rest.find('/')?;
    let total: String = rest[slash + 1..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    total.parse().ok()
}

fn fill_with_wrapped_lines(zellij: &TestSession) {
    let terminal = claim_first_terminal_and_wait_for_prompt(zellij);
    // Each line is ~3x the viewport width, so it occupies ~3 display lines.
    let wide = "w".repeat(TERMINAL_SIZE.cols * 3);
    for i in 1..=60 {
        terminal.output(format!("{:03}-{}\r\n", i, wide).as_bytes());
    }
    terminal.output(format!("{}\r\n", LAST_LINE_MARKER).as_bytes());
    zellij.wait_until("pane filled with wrapped lines", |grid_snapshot| {
        grid_snapshot.contains(LAST_LINE_MARKER)
    });
}

#[test]
fn scrollback_counter_respects_display_line_limit() {
    let mut zellij = TestRunner::new(TERMINAL_SIZE)
        .with_config(&format!("scroll_buffer_size {}", SCROLL_BUFFER_SIZE))
        .start();

    fill_with_wrapped_lines(&zellij);

    // Enter scroll mode and scroll up so the indicator renders.
    zellij.send_stdin(&keys::ctrl('s'));
    zellij.send_stdin(&keys::ctrl('b'));

    let grid_snapshot = zellij.wait_until("scroll indicator is visible", |grid_snapshot| {
        scroll_total(&grid_snapshot.to_string()).is_some()
    });

    let rendered = grid_snapshot.to_string();
    let total = scroll_total(&rendered).expect("scroll indicator should report a total");

    assert!(
        total <= SCROLL_BUFFER_SIZE,
        "scrollback total {} exceeded the configured scroll_buffer_size {}; \
         display-line accounting has drifted.\n{}",
        total,
        SCROLL_BUFFER_SIZE,
        rendered
    );

    zellij.quit();
}
