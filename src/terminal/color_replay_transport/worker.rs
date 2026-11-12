//! Isolated, bounded replay worker orchestration. No original-source capture guard.
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};
const CORPUS_LIMIT: u64 = 128 * 1024;
pub const WORKER: &str = "terminal::color_oracles::replay_actual_worker";
pub const MARKER: &str = "FROST_COLOR_SEALED_REPLAY_CHILD";
pub const CORPUS_SHA: &str = "816b056db14d6391580c2812f6b43f88ac27a6ee5bc35997b1c6a6e9a8ee0759";
fn fail(s: impl Into<String>) -> io::Error {
    io::Error::other(s.into())
}
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Spec<'a> {
    name: &'a str,
    marker: &'a str,
    hash: &'a str,
    listing_deadline: Duration,
    worker_deadline: Duration,
    diagnostic_limit: u64,
    actual_limit: u64,
}
fn default_spec() -> Spec<'static> {
    Spec {
        name: WORKER,
        marker: MARKER,
        hash: CORPUS_SHA,
        listing_deadline: Duration::from_secs(30),
        worker_deadline: Duration::from_secs(1800),
        diagnostic_limit: 1024 * 1024,
        actual_limit: super::ACTUAL_COMPRESSED_LIMIT,
    }
}
fn fresh(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}
fn check_paths(paths: &[(&Path, u64)]) -> io::Result<()> {
    for (path, max) in paths {
        match fs::symlink_metadata(path) {
            Ok(m) => {
                if !m.is_file() || m.len() > *max {
                    return Err(fail(format!("output type/size bound: {}", path.display())));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
fn wait(
    mut child: ChildGuard,
    deadline: Duration,
    paths: &[(&Path, u64)],
) -> io::Result<ExitStatus> {
    let start = Instant::now();
    loop {
        check_paths(paths)?;
        if let Some(status) = child.0.try_wait()? {
            check_paths(paths)?;
            return Ok(status);
        }
        if start.elapsed() > deadline {
            return Err(fail("child deadline exceeded"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
/// Parent and child both verify the exact same bounded input corpus.
pub fn read_corpus(path: &Path) -> io::Result<Vec<u8>> {
    checked_corpus(path, CORPUS_SHA)
}
fn checked_corpus(path: &Path, hash: &str) -> io::Result<Vec<u8>> {
    let f = File::open(path)?;
    if f.metadata()?.len() > CORPUS_LIMIT {
        return Err(fail("corpus length"));
    }
    let mut bytes = Vec::new();
    f.take(CORPUS_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > CORPUS_LIMIT || format!("{:x}", Sha256::digest(&bytes)) != hash {
        return Err(fail("corpus SHA/length"));
    }
    Ok(bytes)
}
/// `dir` must be a new caller-owned private scratch directory. Never use fixture dir.
pub fn run(corpus: &Path, dir: &Path) -> io::Result<PathBuf> {
    let binary = std::env::current_exe()?.canonicalize()?;
    if !binary.is_absolute() {
        return Err(fail("absolute current_exe required"));
    }
    run_spec(&binary, corpus, dir, &default_spec())
}
fn run_spec(binary: &Path, corpus: &Path, dir: &Path, s: &Spec<'_>) -> io::Result<PathBuf> {
    if std::env::var_os(s.marker).is_some() {
        return Err(fail("recursive worker invocation"));
    }
    if !fs::symlink_metadata(dir)?.is_dir() {
        return Err(fail("scratch directory required"));
    }
    let dir = dir.canonicalize()?;
    // Copy verified immutable input into new private file; child independently hashes it.
    let bytes = checked_corpus(corpus, s.hash)?;
    let input = dir.join("replay-corpus.json");
    fresh(&input)?.write_all(&bytes)?;
    let list = dir.join("worker-list.txt");
    let list_err = dir.join("worker-list.stderr");
    let child = Command::new(binary)
        .args(["--list", "--ignored", "--exact", s.name])
        .stdin(Stdio::null())
        .stdout(Stdio::from(fresh(&list)?))
        .stderr(Stdio::from(fresh(&list_err)?))
        .spawn()?;
    let status = wait(
        ChildGuard(child),
        s.listing_deadline,
        &[(&list, 4096), (&list_err, s.diagnostic_limit)],
    )?;
    if !status.success() {
        return Err(fail("worker listing failed"));
    }
    let mut text = String::new();
    File::open(&list)?.take(4097).read_to_string(&mut text)?;
    if text.len() > 4096
        || text
            .lines()
            .filter(|x| x.ends_with(": test"))
            .collect::<Vec<_>>()
            != vec![format!("{}: test", s.name)]
    {
        return Err(fail("exact worker name mismatch"));
    }
    let actual = dir.join("actual.jsonl.zst");
    if actual.try_exists()? {
        return Err(fail("actual must be new"));
    }
    let stdout = dir.join("worker.stdout");
    let stderr = dir.join("worker.stderr");
    let child = Command::new(binary)
        .args([
            "--ignored",
            "--exact",
            s.name,
            "--test-threads=1",
            "--nocapture",
        ])
        .env(s.marker, "v1")
        .env("FROST_COLOR_ALLOCATOR_FIRST", "1")
        .env("FROST_COLOR_REPLAY_CORPUS", &input)
        .env("FROST_COLOR_REPLAY_ACTUAL", &actual)
        .stdin(Stdio::null())
        .stdout(Stdio::from(fresh(&stdout)?))
        .stderr(Stdio::from(fresh(&stderr)?))
        .spawn()?;
    let status = wait(
        ChildGuard(child),
        s.worker_deadline,
        &[
            (&stdout, s.diagnostic_limit),
            (&stderr, s.diagnostic_limit),
            (&actual, s.actual_limit),
        ],
    )?;
    if !status.success() {
        return Err(fail(format!(
            "worker failed: {status}; diagnostics in {}",
            dir.display()
        )));
    }
    if fs::metadata(&actual)?.len() == 0 {
        return Err(fail("empty actual stream"));
    }
    Ok(actual)
}
/// Called only by the ignored replay test in the fresh test-binary subprocess.
/// Caller parses corpus and invokes the unchanged actual-state emitter in `emit`.
pub fn child(emit: impl FnOnce(&[u8], &mut dyn Write) -> io::Result<()>) -> io::Result<()> {
    if std::env::var(MARKER).as_deref() != Ok("v1")
        || std::env::var("FROST_COLOR_ALLOCATOR_FIRST").as_deref() != Ok("1")
    {
        return Err(fail("private worker guard"));
    }
    let input =
        std::env::var_os("FROST_COLOR_REPLAY_CORPUS").ok_or_else(|| fail("missing corpus path"))?;
    let output =
        std::env::var_os("FROST_COLOR_REPLAY_ACTUAL").ok_or_else(|| fail("missing actual path"))?;
    let bytes = read_corpus(Path::new(&input))?;
    super::write_actual(fresh(Path::new(&output))?, |w| emit(&bytes, w))
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        dir: PathBuf,
        bin: PathBuf,
        corpus: PathBuf,
        hash: String,
        marker: String,
    }
    impl Fixture {
        fn new(mode: &str) -> Self {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("frost-worker-proof-{}-{n}", std::process::id()));
            fs::create_dir(&dir).unwrap();
            let bin = dir.join("fake.sh");
            let corpus = dir.join("input");
            fs::write(&corpus, b"{}\n").unwrap();
            let hash = format!("{:x}", Sha256::digest(b"{}\n"));
            let script = format!(
                r#"#!/bin/sh
printf '%s' "$$" > '{}/last.pid'
if [ "$1" = '--list' ]; then
 case '{}' in listhang) exec sleep 10;; listlarge) head -c 5000 /dev/zero;; mismatch) echo 'wrong: test';; *) echo '{}: test';; esac
 exit 0
fi
case '{}' in
 nonzero) echo problem >&2; exit 7;;
 hang) exec sleep 10;;
 large) head -c 2048 /dev/zero > "$FROST_COLOR_REPLAY_ACTUAL"; exec sleep 10;;
 diagnostic) head -c 2048 /dev/zero >&2; exec sleep 10;;
 *) printf actual > "$FROST_COLOR_REPLAY_ACTUAL"; echo ok; echo warning >&2;;
esac
"#,
                dir.display(),
                mode,
                WORKER,
                mode
            );
            fs::write(&bin, script).unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                dir,
                bin,
                corpus,
                hash,
                marker: format!("FROST_TEST_CHILD_{}_{n}", std::process::id()),
            }
        }
        fn run(&self) -> io::Result<PathBuf> {
            run_spec(
                &self.bin,
                &self.corpus,
                &self.dir,
                &Spec {
                    name: WORKER,
                    marker: &self.marker,
                    hash: &self.hash,
                    listing_deadline: Duration::from_millis(150),
                    worker_deadline: Duration::from_millis(150),
                    diagnostic_limit: 1024,
                    actual_limit: 1024,
                },
            )
        }
        fn reaped(&self) {
            let p = fs::read_to_string(self.dir.join("last.pid")).unwrap();
            assert!(
                !Path::new("/proc").join(p.trim()).exists(),
                "direct child leaked"
            );
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
    #[test]
    fn success_diagnostics_and_create_new() {
        let f = Fixture::new("ok");
        let p = f.run().unwrap();
        assert_eq!(fs::read(p).unwrap(), b"actual");
        assert_eq!(fs::read(f.dir.join("worker.stdout")).unwrap(), b"ok\n");
        assert_eq!(fs::read(f.dir.join("worker.stderr")).unwrap(), b"warning\n");
        f.reaped();
        assert!(f.run().is_err());
    }
    #[test]
    fn failures_reap_children() {
        for mode in [
            "nonzero",
            "hang",
            "large",
            "diagnostic",
            "listhang",
            "listlarge",
            "mismatch",
        ] {
            let f = Fixture::new(mode);
            assert!(f.run().is_err(), "{mode}");
            f.reaped();
        }
    }
    #[test]
    fn rejects_recursive_corpus_and_diagnostic_clobber() {
        let f = Fixture::new("ok");
        std::env::set_var(&f.marker, "v1");
        assert!(f.run().is_err());
        std::env::remove_var(&f.marker);
        assert!(!f.dir.join("last.pid").exists());
        fs::write(&f.corpus, b"bad").unwrap();
        assert!(f.run().is_err());
        assert!(!f.dir.join("last.pid").exists());
        let f = Fixture::new("ok");
        fs::write(f.dir.join("worker-list.stderr"), b"keep").unwrap();
        assert!(f.run().is_err());
        assert_eq!(fs::read(f.dir.join("worker-list.stderr")).unwrap(), b"keep");
        assert!(!f.dir.join("last.pid").exists());
    }
}
