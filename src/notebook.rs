//! Explicitly opened local notebooks in the Files inspector.
use crate::Message;
use iced::widget::{button, column, row, scrollable, text};
use iced::{Element, Font, Length, Task};
use jterm_core::notebook::{
    CellOutcome, CellRun, CellRunView, NotebookDocument, NotebookPart, MAX_LOCAL_NOTEBOOK_BYTES,
};
use jterm_core::review_input::{safe_inline_display, safe_multiline_display};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

const MAX_CONCURRENT_CELLS: usize = 8;

pub(crate) fn is_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".jtnb.md"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageId {
    session: usize,
    token: u64,
}

#[derive(Clone)]
pub(crate) struct Snapshot(Arc<Document>);

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NotebookSnapshot")
            .finish_non_exhaustive()
    }
}

struct Document {
    source: NotebookDocument,
    display: Vec<String>,
}

impl Snapshot {
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|_| is_path(path))
            .ok_or("Choose a local .jtnb.md notebook")?;
        let bytes = crate::file_preview::read_tree_notebook(path)?;
        let source = NotebookDocument::open_local_snapshot(name.to_owned(), &bytes)
            .map_err(|error| error.to_string())?;
        let display = source
            .view()
            .parts
            .iter()
            .map(|part| {
                let value = match part {
                    NotebookPart::Text { text } => text,
                    NotebookPart::Cell { source, .. } => source,
                };
                display_text(value)
            })
            .collect();
        Ok(Self(Arc::new(Document { source, display })))
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Action {
    Close,
    Run(usize),
    Stop(usize),
    CopyStdout(usize),
    CopyStderr(usize),
}

#[derive(Default)]
pub(crate) struct Panel {
    page: Option<Page>,
    reader: Option<PageId>,
    token: u64,
    stopping: Vec<CellRun>,
}

struct Page {
    id: PageId,
    result: Option<Result<Snapshot, String>>,
    cells: HashMap<usize, Cell>,
    notice: Option<String>,
}

struct Cell {
    job: Option<CellRun>,
    receipt: CellRunView,
}

impl Cell {
    fn new(job: CellRun) -> Self {
        let receipt = safe_receipt(job.view());
        Self {
            job: Some(job),
            receipt,
        }
    }

    fn poll(&mut self) {
        let Some(job) = &self.job else { return };
        if let Some(receipt) = job.view_after(self.receipt.revision) {
            self.receipt = safe_receipt(receipt);
        }
        if !matches!(self.receipt.outcome, CellOutcome::Running) {
            self.job = None;
        }
    }
}

impl Panel {
    pub(crate) fn open(&mut self, session: usize) -> Result<PageId, &'static str> {
        if self.reader.is_some() {
            return Err("A notebook is still opening");
        }
        self.close();
        self.token = self
            .token
            .checked_add(1)
            .ok_or("Notebook open limit reached")?;
        let id = PageId {
            session,
            token: self.token,
        };
        self.reader = Some(id);
        self.page = Some(Page {
            id,
            result: None,
            cells: HashMap::new(),
            notice: None,
        });
        Ok(id)
    }

    pub(crate) fn close(&mut self) {
        if let Some(page) = self.page.take() {
            for job in page.cells.into_values().filter_map(|cell| cell.job) {
                job.stop();
                self.stopping.push(job);
            }
        }
    }

    pub(crate) fn complete(
        &mut self,
        active: Option<usize>,
        id: PageId,
        result: Result<Snapshot, String>,
    ) {
        if self.reader != Some(id) {
            return;
        }
        self.reader = None;
        if let Some(page) = self.matching_page(active, id) {
            page.result = Some(result);
        }
    }

    pub(crate) fn visible_for(&self, active: Option<usize>) -> bool {
        self.page
            .as_ref()
            .is_some_and(|page| Some(page.id.session) == active)
    }

    pub(crate) fn has_jobs(&self) -> bool {
        self.job_count() > 0
    }

    fn job_count(&self) -> usize {
        self.stopping.len()
            + self.page.as_ref().map_or(0, |page| {
                page.cells
                    .values()
                    .filter(|cell| cell.job.is_some())
                    .count()
            })
    }

    pub(crate) fn poll(&mut self) {
        self.stopping.retain(CellRun::is_running);
        if let Some(page) = &mut self.page {
            for cell in page.cells.values_mut() {
                cell.poll();
            }
        }
    }

    fn matching_page(&mut self, active: Option<usize>, id: PageId) -> Option<&mut Page> {
        self.page
            .as_mut()
            .filter(|page| page.id == id && Some(id.session) == active)
    }

    pub(crate) fn update(
        &mut self,
        active: Option<usize>,
        id: PageId,
        action: Action,
    ) -> Task<Message> {
        let capacity = self.job_count() < MAX_CONCURRENT_CELLS;
        let Some(page) = self.matching_page(active, id) else {
            return Task::none();
        };
        if matches!(action, Action::Close) {
            self.close();
            return Task::none();
        }
        let Some(Ok(snapshot)) = &page.result else {
            return Task::none();
        };
        match action {
            Action::Run(index)
                if capacity && page.cells.get(&index).is_none_or(|cell| cell.job.is_none()) =>
            {
                match snapshot.0.source.start(index) {
                    Ok(job) => {
                        page.notice = None;
                        page.cells.insert(index, Cell::new(job));
                    }
                    Err(error) => page.notice = Some(safe_inline_display(&error.to_string(), 768)),
                }
            }
            Action::Stop(index) => {
                if let Some(job) = page.cells.get(&index).and_then(|cell| cell.job.as_ref()) {
                    job.stop();
                }
            }
            Action::CopyStdout(index) | Action::CopyStderr(index) => {
                if let Some(cell) = page.cells.get(&index) {
                    let stream = if matches!(action, Action::CopyStdout(_)) {
                        &cell.receipt.stdout
                    } else {
                        &cell.receipt.stderr
                    };
                    return iced::clipboard::write(stream.clone());
                }
            }
            _ => {}
        }
        Task::none()
    }

    pub(crate) fn view(&self, active: Option<usize>) -> Option<Element<'_, Message>> {
        let page = self
            .page
            .as_ref()
            .filter(|page| Some(page.id.session) == active)?;
        let event = |action| Message::NotebookAction(page.id, action);
        let (name, document) = match &page.result {
            Some(Ok(snapshot)) => (snapshot.0.source.view().name.as_str(), Some(&snapshot.0)),
            _ => ("Notebook", None),
        };
        let header = row![
            text(name).width(Length::Fill),
            button("Close").on_press(event(Action::Close))
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);
        let body = column![header].padding(10).spacing(8);
        let Some(document) = document else {
            return Some(
                match &page.result {
                    Some(Err(error)) => {
                        body.push(text(safe_inline_display(error, 768)).style(text::danger))
                    }
                    _ => body.push(text("Opening notebook…")),
                }
                .into(),
            );
        };
        let mut body = body.push(text("Cells run in a temporary working directory.").size(12));
        if let Some(notice) = &page.notice {
            body = body.push(text(notice.as_str()).style(text::danger));
        }
        let mut parts = column![].spacing(10);
        let capacity = self.job_count() < MAX_CONCURRENT_CELLS;
        for (part, display) in document.source.view().parts.iter().zip(&document.display) {
            let NotebookPart::Cell {
                index, interpreter, ..
            } = part
            else {
                parts = parts.push(text(display.as_str()).size(12));
                continue;
            };
            let index = *index;
            let cell = page.cells.get(&index);
            let mut controls =
                row![text(interpreter.as_deref().unwrap_or("Read-only cell")).size(12)]
                    .spacing(6)
                    .align_y(iced::Alignment::Center);
            if cell.is_some_and(|cell| cell.job.is_some()) {
                controls = controls.push(button("Stop").on_press(event(Action::Stop(index))));
            } else if interpreter.is_some() {
                controls = controls.push(
                    button("Run").on_press_maybe(capacity.then(|| event(Action::Run(index)))),
                );
            }
            parts = parts.push(controls).push(monospace(display));
            if let Some(cell) = cell {
                parts = parts.push(text(status(&cell.receipt.outcome)).size(12));
                if cell.receipt.truncated {
                    parts = parts.push(text("Output limit reached").size(12));
                }
                for (label, stream, action) in [
                    ("stdout", &cell.receipt.stdout, Action::CopyStdout(index)),
                    ("stderr", &cell.receipt.stderr, Action::CopyStderr(index)),
                ] {
                    if !stream.is_empty() {
                        parts = parts
                            .push(
                                row![
                                    text(label).size(12).width(Length::Fill),
                                    button("Copy").on_press(event(action)),
                                ]
                                .align_y(iced::Alignment::Center),
                            )
                            .push(monospace(stream));
                    }
                }
            }
        }
        Some(
            body.push(scrollable(parts).height(Length::Fill))
                .height(Length::Fill)
                .into(),
        )
    }
}

fn monospace(value: &str) -> iced::widget::Text<'_> {
    text(value).font(Font::MONOSPACE).size(12)
}

fn display_text(value: &str) -> String {
    safe_multiline_display(value, MAX_LOCAL_NOTEBOOK_BYTES)
}

fn safe_receipt(mut receipt: CellRunView) -> CellRunView {
    receipt.stdout = display_text(&receipt.stdout);
    receipt.stderr = display_text(&receipt.stderr);
    receipt
}

fn status(outcome: &CellOutcome) -> String {
    let (label, code, signal) = match outcome {
        CellOutcome::Running => return "Running".into(),
        CellOutcome::Exited { code, signal } => ("Exited", code, signal),
        CellOutcome::Stopped { code, signal } => ("Stopped", code, signal),
        CellOutcome::Failed { reason } => return safe_inline_display(reason, 768),
    };
    match (code, signal) {
        (Some(code), _) => format!("{label} (code {code})"),
        (_, Some(signal)) => format!("{label} (signal {signal})"),
        _ => label.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn stale_notebook_controls_cannot_run_a_cell_and_stop_keeps_partial_output() {
        let source = NotebookDocument::open_local_snapshot(
            "fixture.jtnb.md".into(),
            "```sh\nprintf partial-out; printf partial-err >&2; exec sleep 30\n```\n",
        )
        .unwrap();
        let snapshot = Snapshot(Arc::new(Document {
            source,
            display: Vec::new(),
        }));
        let mut panel = Panel::default();
        let old = panel.open(4).unwrap();
        panel.close();
        panel.complete(Some(4), old, Ok(snapshot.clone()));
        assert!(!panel.visible_for(Some(4)));

        let current = panel.open(4).unwrap();
        panel.complete(Some(4), current, Ok(snapshot));
        let _ = panel.update(Some(4), old, Action::Run(0));
        let _ = panel.update(Some(5), current, Action::Run(0));
        assert!(!panel.has_jobs());

        let _ = panel.update(Some(4), current, Action::Run(0));
        assert!(panel.has_jobs());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            panel.poll();
            let receipt = &panel.page.as_ref().unwrap().cells[&0].receipt;
            if receipt.stdout == "partial-out" && receipt.stderr == "partial-err" {
                break;
            }
            assert!(Instant::now() < deadline, "{receipt:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = panel.update(Some(4), current, Action::Stop(0));
        let deadline = Instant::now() + Duration::from_secs(5);
        while panel.has_jobs() {
            panel.poll();
            assert!(Instant::now() < deadline, "cell did not stop and reap");
            std::thread::sleep(Duration::from_millis(10));
        }
        let receipt = &panel.page.as_ref().unwrap().cells[&0].receipt;
        assert!(matches!(receipt.outcome, CellOutcome::Stopped { .. }));
        assert_eq!(receipt.stdout, "partial-out");
        assert_eq!(receipt.stderr, "partial-err");
    }
}
