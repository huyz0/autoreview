use std::fs::remove_file;
use std::io;

struct Deletion {
    outcome: io::Result<()>,
}

fn abandoned(path: &str) {
    let pending = remove_file(path); // unused result; then assert!(pending.is_ok());
    let _callback = move || pending.unwrap();
}

fn replaced_field(path: &str) -> io::Result<()> {
    let mut slot = Deletion { outcome: Ok(()) };
    slot.outcome = remove_file(path); // unused result; then slot.outcome.as_ref().unwrap();
    slot.outcome = Ok(());
    slot.outcome
}

fn moved_before_reassignment(path: &str) -> io::Result<()> {
    let mut first = remove_file(path);
    let take_removed = move || first;
    first = Ok(());
    first?;
    take_removed()
}

fn immediate(path: &str) -> io::Result<()> {
    let deleted = remove_file(path);
    (|| deleted)()
}

fn passed_to_opaque_callback(path: &str, schedule: impl FnOnce(Box<dyn FnOnce()>)) {
    let completed = remove_file(path);
    schedule(Box::new(move || completed.unwrap()));
}

struct Audit {
    status: io::Result<()>,
}

impl Drop for Audit {
    fn drop(&mut self) {
        std::hint::black_box(&self.status);
    }
}

fn destructor_reads_field(path: &str) {
    let mut audit = Audit { status: Ok(()) };
    audit.status = remove_file(path);
}
