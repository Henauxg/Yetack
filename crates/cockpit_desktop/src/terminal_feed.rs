//! Terminal output on its way from a connection to the page, apart from the state: a busy shell
//! writes many times a second, and each change of the state renders the window again.

use std::cell::RefCell;
use std::rc::Rc;

use cockpit_protocol::{AgentId, TerminalId};
use tokio::sync::mpsc;

/// Output a shell wrote: `data`, base64, starts at byte `offset` of all it wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub agent: AgentId,
    pub terminal: TerminalId,
    pub offset: u64,
    pub data: String,
}

/// The connections' end.
#[derive(Debug, Clone)]
pub struct FeedSender(mpsc::UnboundedSender<Output>);

impl FeedSender {
    pub fn send(&self, output: Output) {
        // The window closed: nobody shows it.
        let _ = self.0.send(output);
    }
}

/// The page's end, which the terminal pane's script takes once for the window's life.
#[derive(Debug, Clone)]
pub struct FeedReceiver(Rc<RefCell<Option<mpsc::UnboundedReceiver<Output>>>>);

impl PartialEq for FeedReceiver {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl FeedReceiver {
    pub fn take(&self) -> Option<mpsc::UnboundedReceiver<Output>> {
        self.0.borrow_mut().take()
    }
}

pub fn channel() -> (FeedSender, FeedReceiver) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (
        FeedSender(sender),
        FeedReceiver(Rc::new(RefCell::new(Some(receiver)))),
    )
}
