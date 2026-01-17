use bstr::BString;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Request {
    Quit,
    SubscribeEvents,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Response {
    Ack,
    ReportEvent(Report),
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Report {
    pub kernel_log: BString,
}
impl Report {
    pub fn new(kernel_log: BString) -> Self {
        Self { kernel_log }
    }
}
