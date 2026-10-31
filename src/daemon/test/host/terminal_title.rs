//! A foreground host's terminal title follows its session count and is put back
//! when the host shuts down (docs/operating.md).

use super::*;
use std::sync::Mutex;

/// The terminal, as a buffer the test reads while the titling task writes it.
#[derive(Clone, Default)]
struct Terminal(Arc<Mutex<Vec<u8>>>);

impl io::Write for Terminal {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Terminal {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }

    async fn shows(&self, title: &str) {
        let osc = format!("\x1b]0;{title}\x07");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !self.text().ends_with(&osc) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("never titled {title:?}: {:?}", self.text()));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_terminal_title_counts_sessions_until_shutdown() {
    let roots = Roots::new();
    let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
    let terminal = Terminal::default();
    let titling = tokio::spawn(Arc::clone(&host).title_terminal(terminal.clone()));
    terminal.shows("efference · 0 sessions").await;

    host.adopt(
        "one".into(),
        SessionRuntime::start(runtime::boot(&roots.0.join("one")), host.swansong()),
    )
    .expect("the id is free in this test");
    terminal.shows("efference · 1 session").await;

    host.swansong().shut_down().await;
    titling.await.unwrap();
    let text = terminal.text();
    assert!(text.starts_with("\x1b[22;0t"), "saved first: {text:?}");
    assert!(text.ends_with("\x1b[23;0t"), "restored last: {text:?}");
}
